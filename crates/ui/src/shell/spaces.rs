//! Spaces sidebar: the space-filter dropdown (searchable, with "All projects"),
//! the filtered Sessions list, and the add-space palette (device
//! tabs + filtered folder browser).
//!
//! A space = a synced (device, folder) pair. Spaces stopped being a
//! navigation spine when tabs went device-local: the dropdown only FILTERS
//! the sidebar's session list (never the tab strip) and hosts space
//! management (add via the palette; rename/delete via row context menus).
//! Child module of `shell` so it renders straight off `Shell`'s private state.

use super::*;
use crate::pickers::{breadcrumbs, browser_rows, completion_prefix_len, parent_path};
use gpui::{FocusHandle, Window};
use luna_proto::{ChatIndicator, Device, DriveEntry, DriveListing, FolderListing, Space};

struct ActiveChatRow {
    depth: usize,
    status: ChatIndicator,
    chat: luna_proto::Chat,
    can_settle: bool,
    folder: String,
    branch: Option<String>,
    change_request: Option<luna_proto::ChangeRequestSummary>,
    group: Option<(String, String)>,
}

/// The sidebar parent of `chat`: its child-thread parent when that row is in
/// the same list, device, project and pin scope. Filtering, pinning or a
/// missing/archived parent leaves the child a reachable root.
fn lineage_parent<'a>(
    rows: &std::collections::HashMap<&str, &'a luna_proto::Chat>,
    chat: &luna_proto::Chat,
) -> Option<&'a luna_proto::Chat> {
    let id = &chat.child_thread.as_ref()?.parent_task_id;
    let root = *rows.get(id.as_str())?;
    (root.id != chat.id
        && root.device_id == chat.device_id
        && root.space_id == chat.space_id
        && root.pinned_order.is_some() == chat.pinned_order.is_some())
    .then_some(root)
}

/// Child chats are listed in their parent's right-pane Child chats surface,
/// not the sidebar. Returns each folded child mapped to its nearest visible
/// ancestor. The selected chat and its ancestors stay visible, so a child
/// opened directly (search, notification, "Open as main chat") still shows,
/// nested under its parent. Children whose parent is not in `chats`
/// (archived, deleted, filtered out, other pin scope) and malformed or cyclic
/// lineage are never folded.
fn folded_child_chats(
    chats: &[&luna_proto::Chat],
    selected: Option<&str>,
) -> std::collections::HashMap<String, String> {
    use std::collections::{HashMap, HashSet};
    let rows: HashMap<&str, &luna_proto::Chat> =
        chats.iter().map(|chat| (chat.id.as_str(), *chat)).collect();
    let mut revealed = HashSet::new();
    let mut cursor = selected.and_then(|id| rows.get(id).copied());
    while let Some(chat) = cursor {
        if !revealed.insert(chat.id.as_str()) {
            break;
        }
        cursor = lineage_parent(&rows, chat);
    }
    let reaches_root = |chat: &luna_proto::Chat| {
        let mut seen = HashSet::new();
        let mut cursor = chat;
        loop {
            if seen.len() > 32 || !seen.insert(cursor.id.as_str()) {
                return false;
            }
            match lineage_parent(&rows, cursor) {
                Some(parent) => cursor = parent,
                None => return true,
            }
        }
    };
    let mut folded = HashMap::new();
    for chat in chats {
        let Some(mut ancestor) = lineage_parent(&rows, chat) else {
            continue;
        };
        if revealed.contains(chat.id.as_str()) || !reaches_root(chat) {
            continue;
        }
        while !revealed.contains(ancestor.id.as_str())
            && let Some(parent) = lineage_parent(&rows, ancestor)
        {
            ancestor = parent;
        }
        folded.insert(chat.id.clone(), ancestor.id.clone());
    }
    folded
}

/// Remove folded child chats from sidebar rows. A parent row carries the most
/// urgent status of the children folded into it, so a child that needs
/// approval or input (or is still working) keeps its parent visible and
/// unsettled.
fn fold_child_chats<'a>(
    rows: Vec<(ChatIndicator, &'a luna_proto::Chat)>,
    selected: Option<&str>,
) -> Vec<(ChatIndicator, &'a luna_proto::Chat)> {
    let chats: Vec<&luna_proto::Chat> = rows.iter().map(|(_, chat)| *chat).collect();
    let folded = folded_child_chats(&chats, selected);
    if folded.is_empty() {
        return rows;
    }
    let mut summary: std::collections::HashMap<&str, ChatIndicator> =
        std::collections::HashMap::new();
    for (status, chat) in &rows {
        if let Some(ancestor) = folded.get(&chat.id) {
            let entry = summary
                .entry(ancestor.as_str())
                .or_insert(ChatIndicator::Idle);
            if child_indicator_priority(*status) > child_indicator_priority(*entry) {
                *entry = *status;
            }
        }
    }
    rows.iter()
        .filter(|(_, chat)| !folded.contains_key(&chat.id))
        .map(|(status, chat)| {
            let status = match summary.get(chat.id.as_str()) {
                Some(child)
                    if child_indicator_priority(*child) > child_indicator_priority(*status) =>
                {
                    *child
                }
                _ => *status,
            };
            (status, *chat)
        })
        .collect()
}

/// One tree projection shared by rendering and keyboard navigation. After
/// [`fold_child_chats`] the only nested rows left are the selected chat's
/// lineage; malformed or cyclic imported lineage must never hide a row.
struct ChildVisibleRow {
    id: String,
    depth: usize,
}
fn child_visible_tree(chats: &[luna_proto::Chat]) -> Vec<ChildVisibleRow> {
    use std::collections::{HashMap, HashSet};
    let rows: HashMap<&str, &luna_proto::Chat> =
        chats.iter().map(|chat| (chat.id.as_str(), chat)).collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for chat in chats {
        if let Some(parent) = lineage_parent(&rows, chat) {
            children
                .entry(parent.id.as_str())
                .or_default()
                .push(chat.id.as_str());
        }
    }
    fn visit<'a>(
        id: &'a str,
        depth: usize,
        children: &HashMap<&'a str, Vec<&'a str>>,
        visited: &mut HashSet<&'a str>,
        output: &mut Vec<ChildVisibleRow>,
    ) {
        if depth > 32 || !visited.insert(id) {
            return;
        }
        output.push(ChildVisibleRow {
            id: id.into(),
            depth,
        });
        for &child in children.get(id).into_iter().flatten() {
            visit(child, depth + 1, children, visited, output);
        }
    }
    let mut output = Vec::new();
    let mut visited = HashSet::new();
    for chat in chats
        .iter()
        .filter(|chat| lineage_parent(&rows, chat).is_none())
    {
        visit(&chat.id, 0, &children, &mut visited, &mut output);
    }
    for chat in chats {
        visit(&chat.id, 0, &children, &mut visited, &mut output);
    }
    output
}

/// Pagination selects families. Keep a visible (selected) child and its
/// available ancestors together without resurrecting archived rows or
/// crossing the filter/pin scope. Folded children are already absent.
fn include_child_families<'a>(
    mut selected: Vec<(ChatIndicator, &'a luna_proto::Chat)>,
    available: Vec<(ChatIndicator, &'a luna_proto::Chat)>,
) -> Vec<(ChatIndicator, &'a luna_proto::Chat)> {
    let mut ids: std::collections::HashSet<String> =
        selected.iter().map(|(_, chat)| chat.id.clone()).collect();
    loop {
        let mut added = false;
        for row in &available {
            if ids.contains(&row.1.id) {
                continue;
            }
            let related = selected.iter().any(|(_, chat)| {
                chat.device_id == row.1.device_id
                    && chat.space_id == row.1.space_id
                    && chat.pinned_order.is_some() == row.1.pinned_order.is_some()
                    && (chat
                        .child_thread
                        .as_ref()
                        .is_some_and(|child| child.parent_task_id == row.1.id)
                        || row
                            .1
                            .child_thread
                            .as_ref()
                            .is_some_and(|child| child.parent_task_id == chat.id))
            });
            if related {
                ids.insert(row.1.id.clone());
                selected.push(*row);
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    selected
}

fn child_indicator_priority(status: ChatIndicator) -> u8 {
    match status {
        ChatIndicator::AwaitingApproval | ChatIndicator::AwaitingInput => 5,
        ChatIndicator::Errored => 4,
        ChatIndicator::Working => 3,
        ChatIndicator::Completed => 2,
        ChatIndicator::Idle => 1,
    }
}

pub(super) fn sidebar_thread_tooltip(
    chat: &luna_proto::Chat,
    title: SharedString,
    project: SharedString,
    device: SharedString,
    branch: Option<SharedString>,
    status: Option<SharedString>,
) -> SidebarThreadTooltip {
    let model = chat.config.as_ref().map(|config| {
        let provider = match config.harness {
            luna_proto::HarnessId::Codex => "Codex",
            luna_proto::HarnessId::ClaudeCode => "Claude Code",
            luna_proto::HarnessId::Opencode => "OpenCode",
            _ => "Provider",
        };
        match config.model.as_deref() {
            Some(model) if !model.is_empty() => format!("{provider} · {model}").into(),
            _ => SharedString::from(provider),
        }
    });
    SidebarThreadTooltip {
        title,
        project,
        device,
        model,
        branch,
        status,
    }
}

pub(super) fn compare_sidebar_chats(
    sort: SidebarSort,
    left: &luna_proto::Chat,
    right: &luna_proto::Chat,
) -> std::cmp::Ordering {
    compare_sidebar_chats_ranked(sort, &ManualRanks::new(), left, right)
}

/// Saved drag positions of unpinned tasks: chat id → index in
/// [`crate::settings::UiSettings::sidebar_manual_order`].
pub(super) type ManualRanks = std::collections::HashMap<String, usize>;

pub(super) fn manual_ranks(order: &[String]) -> ManualRanks {
    order
        .iter()
        .enumerate()
        .map(|(ix, id)| (id.clone(), ix))
        .collect()
}

/// Pins always lead in their synced order. Manual sort follows the saved drag
/// order; tasks it has not seen yet (new, unarchived, or unpinned since) lead
/// newest first, so fresh work is never buried under an old arrangement.
pub(super) fn compare_sidebar_chats_ranked(
    sort: SidebarSort,
    ranks: &ManualRanks,
    left: &luna_proto::Chat,
    right: &luna_proto::Chat,
) -> std::cmp::Ordering {
    match (&left.pinned_order, &right.pinned_order) {
        (Some(left_order), Some(right_order)) => {
            return luna_proto::compare_pin_order(left_order, right_order)
                .then_with(|| left.id.cmp(&right.id));
        }
        (Some(_), None) => return std::cmp::Ordering::Less,
        (None, Some(_)) => return std::cmp::Ordering::Greater,
        (None, None) => {}
    }
    let primary = match sort {
        SidebarSort::Manual => match (ranks.get(&left.id), ranks.get(&right.id)) {
            (Some(left), Some(right)) => left.cmp(right),
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, None) => right.created_at.cmp(&left.created_at),
        },
        SidebarSort::Created => right.created_at.cmp(&left.created_at),
        SidebarSort::LastUpdated => right
            .last_message_at
            .unwrap_or(right.created_at)
            .cmp(&left.last_message_at.unwrap_or(left.created_at)),
    };
    primary.then_with(|| left.id.cmp(&right.id))
}

const ACTIVE_PAGE: usize = 20;
const SETTLED_INITIAL: usize = 10;
const SETTLED_PAGE: usize = 25;

pub(super) fn can_manually_settle_chat(
    chat: &luna_proto::Chat,
    status: ChatIndicator,
    has_attention: bool,
    has_queued_message: bool,
) -> bool {
    !chat.archived
        && chat.pinned_order.is_none()
        && !has_attention
        && !has_queued_message
        && matches!(status, ChatIndicator::Idle | ChatIndicator::Completed)
}

pub(super) fn is_settled_chat(
    chat: &luna_proto::Chat,
    status: ChatIndicator,
    now: chrono::DateTime<Utc>,
    auto_settle_after_days: Option<u16>,
    has_attention: bool,
) -> bool {
    if chat.archived
        || chat.pinned_order.is_some()
        || has_attention
        || !matches!(status, ChatIndicator::Idle | ChatIndicator::Completed)
    {
        return false;
    }
    let last_activity = chat.last_message_at.unwrap_or(chat.created_at);
    if let Some(override_state) = &chat.settlement_override
        && override_state.at >= last_activity
    {
        return override_state.disposition == luna_proto::SettlementDisposition::Settled;
    }
    // A completed turn can be settled deliberately, but only an idle task
    // qualifies for age-based automatic settling.
    if status != ChatIndicator::Idle {
        return false;
    }
    let Some(days) = auto_settle_after_days else {
        return false;
    };
    now.signed_duration_since(last_activity) >= chrono::Duration::days(i64::from(days))
}

/// Bound row creation even when a user makes hundreds of threads in a day.
/// The selected row remains reachable after navigation from search.
fn active_page<'a>(
    chats: Vec<(ChatIndicator, &'a luna_proto::Chat)>,
    selected_id: Option<&str>,
    now: chrono::DateTime<Utc>,
    auto_settle_after_days: Option<u16>,
    attention_task_keys: &std::collections::HashSet<(String, String)>,
    pending_actions: &std::collections::HashMap<String, TaskConfirmation>,
    shown: usize,
) -> (Vec<(ChatIndicator, &'a luna_proto::Chat)>, usize) {
    let active: Vec<_> = chats
        .into_iter()
        .filter(|(status, chat)| match pending_actions.get(&chat.id) {
            Some(
                TaskConfirmation::Archive | TaskConfirmation::Settle | TaskConfirmation::Delete,
            ) => false,
            None => !is_settled_chat(
                chat,
                *status,
                now,
                auto_settle_after_days,
                attention_task_keys.contains(&(chat.device_id.clone(), chat.id.clone())),
            ),
        })
        .collect();
    let limit = shown.max(ACTIVE_PAGE);
    let total = active.len();
    let mut visible: Vec<_> = active.iter().copied().take(limit).collect();
    if let Some(selected) = active
        .iter()
        .skip(limit)
        .find(|(_, chat)| Some(chat.id.as_str()) == selected_id)
    {
        visible.push(*selected);
    }
    let hidden = total.saturating_sub(visible.len());
    (visible, hidden)
}

/// Activity is a view of the existing task rows, not a second inbox. It shows
/// unresolved attention, live work, and recent completions. A stale session
/// has fallen back to Idle upstream.
fn activity_sidebar_bucket(status: ChatIndicator, has_attention: bool) -> Option<u8> {
    if has_attention
        || matches!(
            status,
            ChatIndicator::AwaitingApproval
                | ChatIndicator::AwaitingInput
                | ChatIndicator::Errored
                | ChatIndicator::Completed
        )
    {
        Some(0)
    } else if status == ChatIndicator::Working {
        Some(1)
    } else {
        None
    }
}

#[cfg(test)]
fn activity_sidebar_chats<'a>(
    rows: Vec<(ChatIndicator, &'a luna_proto::Chat)>,
    attention_task_keys: &std::collections::HashSet<(String, String)>,
) -> Vec<(ChatIndicator, &'a luna_proto::Chat)> {
    activity_sidebar_chats_with_recent(rows, attention_task_keys, &Default::default())
}

fn activity_sidebar_chats_with_recent<'a>(
    rows: Vec<(ChatIndicator, &'a luna_proto::Chat)>,
    attention_task_keys: &std::collections::HashSet<(String, String)>,
    recent_completion_keys: &std::collections::HashSet<(String, String)>,
) -> Vec<(ChatIndicator, &'a luna_proto::Chat)> {
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter_map(|(status, chat)| {
            let key = (chat.device_id.clone(), chat.id.clone());
            let review_status = if status == ChatIndicator::Errored && !chat.unseen() {
                ChatIndicator::Idle
            } else {
                status
            };
            activity_sidebar_bucket(review_status, attention_task_keys.contains(&key))
                .or_else(|| recent_completion_keys.contains(&key).then_some(2))
                .map(|bucket| (bucket, status, chat))
        })
        .collect();
    rows.sort_by(|(left_bucket, _, left), (right_bucket, _, right)| {
        left_bucket
            .cmp(right_bucket)
            .then_with(|| {
                right
                    .last_message_at
                    .unwrap_or(right.created_at)
                    .cmp(&left.last_message_at.unwrap_or(left.created_at))
            })
            .then_with(|| left.id.cmp(&right.id))
    });
    rows.into_iter()
        .map(|(_, status, chat)| (status, chat))
        .collect()
}

/// The space-filter dropdown, `Some` while open. The same searchable-menu
/// recipe as the composer's ref picker: filter input on top
/// (`PaletteSearch` context so ↑↓/⏎ bubble to the card), ranked substring
/// rows, keyboard highlight.
pub(super) struct SpacesMenu {
    search: Entity<ComposerInput>,
    /// Keyboard highlight — an index into [`Shell::spaces_menu_rows`], or
    /// that list's length when the pinned "New project…" footer holds it.
    active: usize,
    /// Tracked on the card — puts it on the keyboard dispatch path while the
    /// search input holds focus (the structure every working picker uses).
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    _search_events: Subscription,
}

pub(super) struct SidebarViewMenu {
    /// Keyboard cursor. Mouse-opened menus start without one so the persisted
    /// radio/check state is the only selection signal until an arrow key is
    /// pressed.
    active: Option<usize>,
    focus: FocusHandle,
}

struct SidebarViewOptionsTooltip;

impl Render for SidebarViewOptionsTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .shadow_md()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child("Sidebar view options")
    }
}

#[derive(Clone, Copy)]
enum SidebarViewRow {
    ByDevice,
    InOneList,
    LastUpdated,
    Created,
    Manual,
    ShowBranch,
    ShowPullRequest,
    ShowHarness,
    SettleOff,
    Settle1Day,
    Settle3Days,
    Settle7Days,
}

impl SidebarViewRow {
    /// Radio-style presentation choices behave like the project selector and
    /// dismiss after selection. Show toggles stay open for batch changes.
    fn closes_menu(self) -> bool {
        matches!(
            self,
            Self::ByDevice
                | Self::InOneList
                | Self::LastUpdated
                | Self::Created
                | Self::Manual
                | Self::SettleOff
                | Self::Settle1Day
                | Self::Settle3Days
                | Self::Settle7Days
        )
    }
}

const SIDEBAR_VIEW_ROWS: [SidebarViewRow; 12] = [
    SidebarViewRow::ByDevice,
    SidebarViewRow::InOneList,
    SidebarViewRow::LastUpdated,
    SidebarViewRow::Created,
    SidebarViewRow::Manual,
    SidebarViewRow::ShowBranch,
    SidebarViewRow::ShowPullRequest,
    SidebarViewRow::ShowHarness,
    SidebarViewRow::SettleOff,
    SidebarViewRow::Settle1Day,
    SidebarViewRow::Settle3Days,
    SidebarViewRow::Settle7Days,
];

// list items stay tightly related at 2px, while section boundaries use 12px
// (well over 2x the intra-list gap). Disclosure content gets a small 4px
// handoff from its header without leaving dead space while collapsed.
const SIDEBAR_SECTION_GAP: f32 = 12.0;
const SIDEBAR_DISCLOSURE_HEADER_HEIGHT: f32 = 28.0;
const SIDEBAR_DISCLOSURE_BODY_INSET: f32 = 4.0;
const SIDEBAR_DISCLOSURE_SECTION_HEIGHT: f32 =
    SIDEBAR_SECTION_GAP + SIDEBAR_DISCLOSURE_HEADER_HEIGHT;
pub(super) const SIDEBAR_DISCLOSURE_TWEEN_GRACE: std::time::Duration =
    std::time::Duration::from_millis(120);

/// Put this machine's device group first without disturbing the recency-based
/// order of any remote groups. A targeted promotion is more truthful than a
/// full name sort: local context leads, then the user's chosen chat sort wins.
fn promote_local_device_group<T>(
    groups: &mut Vec<(Option<(String, String)>, Vec<T>)>,
    local_device_id: Option<&str>,
) {
    let Some(local_device_id) = local_device_id else {
        return;
    };
    let Some(index) = groups.iter().position(|(group, _)| {
        group
            .as_ref()
            .is_some_and(|(device_id, _)| device_id == local_device_id)
    }) else {
        return;
    };
    if index > 0 {
        let local = groups.remove(index);
        groups.insert(0, local);
    }
}

/// Shared quiet rule for sidebar groups and palette sections.
pub(super) fn sidebar_separator(theme: &Theme) -> gpui::Div {
    div().h(px(1.0)).bg(theme.border.opacity(0.6))
}

pub(super) fn sidebar_disclosure_header(
    theme: &Theme,
    label: SharedString,
    chevron: AnyElement,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(SIDEBAR_DISCLOSURE_HEADER_HEIGHT))
        .px(px(Theme::SPACE_SM))
        .cursor_pointer()
        .child(super::sidebar_faded_label(
            "sidebar-disclosure-label".into(),
            false,
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if theme.appearance.is_light() {
                    theme.text_muted
                } else {
                    theme.text_muted.opacity(0.5)
                })
                .child(label),
        ))
        .child(sidebar_separator(theme).flex_1())
        .child(chevron)
}

/// One activatable row of the open dropdown, in nav order. `AddSpace` names
/// the card's pinned "New project…" footer, not a list row — keyboard nav
/// maps the list-length index to it.
#[derive(Clone, PartialEq)]
pub(super) enum SpacesMenuRow {
    All,
    Space(String),
    AddSpace,
}

/// New project navigates devices, locations, then folders on a command-palette surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectStep {
    Devices,
    Locations,
    Folders,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AddSpaceOrigin {
    Standalone,
    MoveTask { chat_id: String },
}

pub(super) struct AddSpaceFlow {
    id: u64,
    origin: AddSpaceOrigin,
    step: ProjectStep,
    location: Option<(String, Option<String>)>,
    /// The selected device.
    device: Option<Device>,
    /// Filter input; Enter opens a highlighted folder or a typed path. Carries
    /// the tab-completion ghost (the faint suffix ⇥ accepts).
    search: Entity<ComposerInput>,
    browser: Loadable<FolderListing>,
    /// The selected device's mounted drives/volumes.
    /// Best-effort: an error just leaves the section at Home only.
    drives: Loadable<Vec<DriveEntry>>,
    /// Requested browser path (`None` = the device's default, i.e. home).
    browser_path: Option<String>,
    /// The device's home (the path a `None` browse resolved to) — breadcrumbs
    /// fold everything up to here into the Home crumb.
    home: Option<String>,
    /// Best-effort git seed for the CURRENT browser path (known when we
    /// descended through an entry whose `is_repo` we saw; the owning device's
    /// SpacesSync re-verifies either way).
    browser_repo: bool,
    /// Keyboard highlight within the current step’s filtered rows.
    active: usize,
    submit_busy: bool,
    error: Option<SharedString>,
    /// Tracked on the card (`track_focus`) — puts the card on the keyboard
    /// dispatch path so ↑↓/⌫/esc reach `add_space_key` while the search input
    /// holds focus (the structure every working picker uses).
    focus: FocusHandle,
    /// Folder-list scroll — keyboard navigation keeps the highlighted row in
    /// view (`scroll_to_item`).
    list_scroll: gpui::ScrollHandle,
    focus_pending: bool,
    load_task: Option<Task<()>>,
    drives_task: Option<Task<()>>,
    submit_task: Option<Task<()>>,
    folder_request: u64,
    drives_request: u64,
    pending_home_path: Option<String>,
    _search_events: Subscription,
}

/// Segment-aware "is `path` at or under `base`" (`/media/a` is not under
/// `/media/ab`); a root base covers everything.
fn path_under(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('/');
    base.is_empty() || path == base || path.starts_with(&format!("{base}/"))
}

fn is_project_path_query(query: &str) -> bool {
    query.starts_with('/') || query == "~" || query.starts_with("~/")
}

fn existing_project_id(spaces: &[Space], device_id: &str, path: &str) -> Option<String> {
    spaces
        .iter()
        .find(|space| space.device_id == device_id && space.path == path)
        .map(|space| space.id.clone())
}

/// The space-row Rename dialog (same shape as [`RenameChatDialog`]).
pub(super) struct RenameSpaceDialog {
    pub space_id: String,
    pub input: Entity<ComposerInput>,
    pub focus_pending: bool,
    pub _events: Subscription,
}

/// Dot color for a chat's display status (tab dots + Sessions rows).
pub(super) fn status_dot_color(status: ChatIndicator, theme: &Theme) -> gpui::Hsla {
    match status {
        // Preset activity tone, not warning amber: running is routine.
        // Non-done statuses sit well below full
        // strength: at full alpha the colored words shouted across the
        // whole sidebar (user request) — only Done keeps its pop.
        ChatIndicator::Working => theme.busy.opacity(0.55),
        ChatIndicator::AwaitingApproval => theme.warning.opacity(0.72),
        // Blue: "asking you a question" must read differently from "busy
        // working" at a glance.
        ChatIndicator::AwaitingInput => theme.accent.opacity(0.6),
        ChatIndicator::Errored => theme.danger.opacity(0.65),
        // Green: finished-but-unseen reads as "ready for you".
        ChatIndicator::Completed => {
            theme.success.opacity(0.9) // emerald-400
        }
        ChatIndicator::Idle => crate::theme::ink(0.14),
    }
}

// Handle-based rail host for the spaces dropdown: its list is a plain
// tracked scroller, so the trait's default metrics/press/drag (off the live
// ScrollHandle) apply unchanged.
impl popover::ScrollRailHost for Shell {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        &mut self.spaces_menu_bar
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.spaces_menu.get().map(|menu| menu.list_scroll.clone())
    }
}

impl Shell {
    fn begin_sidebar_disclosure_motion(
        &mut self,
        key: &str,
        resting_height: f32,
        target_height: f32,
    ) {
        let previous = self.sidebar_disclosure_motion.get(key).copied();
        let from = previous
            .filter(|motion| motion.animating())
            .map(SidebarDisclosureMotion::current)
            .unwrap_or(resting_height);
        let epoch = previous.map_or(1, |motion| motion.epoch + 1);
        self.sidebar_disclosure_motion.insert(
            key.to_owned(),
            SidebarDisclosureMotion::new(epoch, from, target_height),
        );
    }

    fn render_sidebar_disclosure_body(
        &self,
        key: &str,
        open: bool,
        full_height: f32,
        content: AnyElement,
    ) -> AnyElement {
        let target = if open { full_height } else { 0.0 };
        let frame = div().w_full().flex_none().overflow_hidden().child(content);
        let Some(tween) = self
            .sidebar_disclosure_motion
            .get(key)
            .copied()
            .filter(|motion| motion.animating())
        else {
            return frame.h(px(target)).into_any_element();
        };
        let denominator = full_height.max(1.0);
        frame
            .with_animation(
                SharedString::from(format!("sidebar-disclosure-{key}-{}", tween.epoch)),
                motion::COLLAPSE.animation(),
                move |el, t| {
                    let height = motion::lerp(tween.from, tween.to, t);
                    let reveal = (height / denominator).clamp(0.0, 1.0);
                    el.h(px(height))
                        .opacity(0.35 + 0.65 * reveal)
                        .relative()
                        .top(px(-3.0 * (1.0 - reveal)))
                },
            )
            .into_any_element()
    }

    pub(super) fn sidebar_disclosure_chevron(
        &self,
        key: &str,
        open: bool,
        theme: &Theme,
    ) -> AnyElement {
        let resting_reveal = if open { 1.0 } else { 0.0 };
        let chevron = icon(icons::ALT_ARROW_RIGHT).size(px(12.0)).text_color(
            if theme.appearance.is_light() {
                theme.text_muted
            } else {
                theme.text_muted.opacity(0.5)
            },
        );
        if let Some(tween) = self
            .sidebar_disclosure_motion
            .get(key)
            .copied()
            .filter(|motion| motion.animating())
        {
            let denominator = tween.from.max(tween.to).max(1.0);
            let from = (tween.from / denominator).clamp(0.0, 1.0);
            let to = (tween.to / denominator).clamp(0.0, 1.0);
            div()
                .flex_none()
                .size(px(12.0))
                .child(chevron.with_animation(
                    SharedString::from(format!("sidebar-chevron-{key}-{}", tween.epoch)),
                    motion::COLLAPSE.animation(),
                    move |el, t| {
                        let reveal = motion::lerp(from, to, t);
                        el.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                            reveal * 0.25,
                        )))
                    },
                ))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .size(px(12.0))
                .child(
                    chevron.with_transformation(gpui::Transformation::rotate(gpui::percentage(
                        resting_reveal * 0.25,
                    ))),
                )
                .into_any_element()
        }
    }
    // ---- space filter ----

    /// Set the sidebar's session filter (`None` = All spaces). On the
    /// new-session canvas the space context follows the filter — the canvas
    /// default is "the space you're looking at".
    pub(super) fn set_space_filter(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        self.settings.space_filter = filter.clone();
        if self.state.read(cx).selected_chat.is_none() {
            if let Some(space_id) = filter.as_deref() {
                self.state
                    .update(cx, |s, cx| s.select_space(Some(space_id.into()), cx));
            }
        }
        self.reconcile_session_history_scope(false, cx);
        self.close_spaces_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Close the space-filter dropdown through the exit animation (no-op when
    /// it isn't open). Every close path funnels here so the menu always
    /// animates out instead of vanishing.
    pub(super) fn close_spaces_menu(&mut self, cx: &mut Context<Self>) {
        if self.spaces_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.spaces_menu);
            cx.notify();
        }
    }

    fn on_spaces_menu_list_hover(
        &mut self,
        hovered: &bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.spaces_menu_bar.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// Open the new-session canvas in a just-added space, preserving the
    /// sidebar's current project filter.
    pub(super) fn land_in_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.focus_composer(cx);
        // "All" stays as-is; an explicit project filter follows the new
        // project so the first send lands in a visible session.
        if self.settings.space_filter.is_some() {
            self.settings.space_filter = Some(space_id.clone());
        }
        self.settings.last_space_id = Some(space_id.clone());
        self.state.update(cx, |s, cx| {
            s.select_space(Some(space_id), cx);
            s.select_chat(None, cx);
        });
        self.schedule_save(cx);
        cx.notify();
    }

    // ---- sidebar sections ----

    /// The filter's scrollable rows: "All projects", then spaces matching
    /// the search (ranked — `popover::filter_indices`). "All" only shows on
    /// an empty query (searching means hunting a space). The "New project…"
    /// action is not a row here — the card renders it as a pinned footer.
    fn spaces_menu_rows(&self, cx: &App) -> Vec<SpacesMenuRow> {
        let query = self
            .spaces_menu
            .get()
            .map(|menu| menu.search.read(cx).text().to_string())
            .unwrap_or_default();
        let state = self.state.read(cx);
        let spaces = state.spaces_sorted();
        let names: Vec<String> = spaces
            .iter()
            .map(|s| s.display_name().to_string())
            .collect();
        let mut rows: Vec<SpacesMenuRow> = Vec::new();
        if query.trim().is_empty() {
            rows.push(SpacesMenuRow::All);
        }
        rows.extend(
            popover::filter_indices(&query, &names)
                .into_iter()
                .map(|ix| SpacesMenuRow::Space(spaces[ix].id.clone())),
        );
        rows
    }

    fn open_spaces_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_sidebar_view_menu(cx);
        // "PaletteSearch" context: ↑↓/⏎ stay unbound in the input and bubble
        // to the card's key handler.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search projects…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.spaces_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        // The highlight starts ON the current filter row.
        let current = self.settings.space_filter.clone();
        let handle = search.read(cx).focus_handle(cx);
        self.spaces_menu.open(SpacesMenu {
            search,
            active: 0,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            _search_events: search_events,
        });
        // Fresh handle at the top — don't let the stale rail baseline read
        // the reopen as scrolling.
        self.spaces_menu_bar.clear_scroll_baseline();
        let rows = self.spaces_menu_rows(cx);
        let start = match &current {
            None => 0,
            Some(id) => rows
                .iter()
                .position(|row| matches!(row, SpacesMenuRow::Space(s) if s == id))
                .unwrap_or(0),
        };
        if let Some(menu) = self.spaces_menu.open_mut() {
            menu.active = start;
        }
        // Focusable before first paint (the add-space palette's proven order).
        window.focus(&handle, cx);
        cx.notify();
    }

    fn activate_spaces_menu_row(&mut self, row: SpacesMenuRow, cx: &mut Context<Self>) {
        match row {
            SpacesMenuRow::All => self.set_space_filter(None, cx),
            SpacesMenuRow::Space(id) => self.set_space_filter(Some(id), cx),
            SpacesMenuRow::AddSpace => {
                self.close_spaces_menu(cx);
                self.open_add_space(cx);
            }
        }
    }

    /// Dropdown keys (bubbling from the focused search input): ↑↓ navigate,
    /// ⏎ activates the highlighted row, esc closes.
    fn spaces_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // The card stays mounted (and focused) through the exit animation —
        // keys must not drive a dying menu.
        if !self.spaces_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.close_spaces_menu(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let rows = self.spaces_menu_rows(cx);
                // +1: the pinned footer stays in the nav order, exactly as
                // when it was the list's last row.
                let count = rows.len() + 1;
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.spaces_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    // The footer renders below the scroller — only in-list
                    // rows can be scrolled to (the footer index would leave
                    // a request pending against a row that never exists).
                    if menu.active < rows.len() {
                        menu.list_scroll.scroll_to_item(menu.active);
                    }
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.spaces_menu.get().map(|m| m.active).unwrap_or(0);
                let rows = self.spaces_menu_rows(cx);
                // One past the scrollable rows is the pinned footer.
                let row = if active < rows.len() {
                    rows[active].clone()
                } else {
                    SpacesMenuRow::AddSpace
                };
                self.activate_spaces_menu_row(row, cx);
            }
            popover::MenuKey::Backspace | popover::MenuKey::Other => {}
        }
    }

    pub(super) fn close_sidebar_view_menu(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_view_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.sidebar_view_menu);
            cx.notify();
        }
    }

    fn open_sidebar_view_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_spaces_menu(cx);
        let focus = cx.focus_handle();
        self.sidebar_view_menu.open(SidebarViewMenu {
            active: None,
            focus: focus.clone(),
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn activate_sidebar_view_row(&mut self, row: SidebarViewRow, cx: &mut Context<Self>) {
        match row {
            SidebarViewRow::ByDevice => {
                self.settings.sidebar_organization = SidebarOrganization::ByDevice
            }
            SidebarViewRow::InOneList => {
                self.settings.sidebar_organization = SidebarOrganization::InOneList
            }
            SidebarViewRow::LastUpdated => self.settings.sidebar_sort = SidebarSort::LastUpdated,
            SidebarViewRow::Created => self.settings.sidebar_sort = SidebarSort::Created,
            SidebarViewRow::Manual => self.settings.sidebar_sort = SidebarSort::Manual,
            SidebarViewRow::ShowBranch => {
                self.settings.sidebar_show_branch = !self.settings.sidebar_show_branch
            }
            SidebarViewRow::ShowPullRequest => {
                self.settings.sidebar_show_pull_request = !self.settings.sidebar_show_pull_request;
                let visible = self.settings.sidebar_show_pull_request;
                self.state.update(cx, |state, cx| {
                    state.set_change_requests_visible(visible, cx)
                });
            }
            SidebarViewRow::ShowHarness => {
                self.settings.sidebar_show_harness = !self.settings.sidebar_show_harness
            }
            SidebarViewRow::SettleOff => self.settings.sidebar_auto_settle_after_days = None,
            SidebarViewRow::Settle1Day => self.settings.sidebar_auto_settle_after_days = Some(1),
            SidebarViewRow::Settle3Days => self.settings.sidebar_auto_settle_after_days = Some(3),
            SidebarViewRow::Settle7Days => self.settings.sidebar_auto_settle_after_days = Some(7),
        }
        self.schedule_save(cx);
        if row.closes_menu() {
            self.close_sidebar_view_menu(cx);
        }
        cx.notify();
    }

    fn sidebar_view_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        if !self.sidebar_view_menu.is_open() {
            return;
        }
        match popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            popover::MenuKey::Escape => self.close_sidebar_view_menu(cx),
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let up = event.keystroke.key.eq_ignore_ascii_case("arrowup");
                if let Some(menu) = self.sidebar_view_menu.open_mut() {
                    menu.active = popover::menu_step(
                        menu.active,
                        SIDEBAR_VIEW_ROWS.len(),
                        if up { -1 } else { 1 },
                    );
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.sidebar_view_menu.get().and_then(|m| m.active);
                if let Some(row) = active.and_then(|ix| SIDEBAR_VIEW_ROWS.get(ix)).copied() {
                    self.activate_sidebar_view_row(row, cx);
                }
            }
            popover::MenuKey::Backspace | popover::MenuKey::Other => {}
        }
    }

    fn render_sidebar_view_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let Some(menu_state) = self.sidebar_view_menu.get() else {
            return div().into_any_element();
        };
        let active = menu_state.active;
        let focus = menu_state.focus.clone();
        let organization = self.settings.sidebar_organization;
        let sort = self.settings.sidebar_sort;
        let show_harness = self.settings.sidebar_show_harness;
        let show_branch = self.settings.sidebar_show_branch;
        let show_pr = self.settings.sidebar_show_pull_request;
        let auto_settle_after_days = self.settings.sidebar_auto_settle_after_days;

        let labels = [
            "By device",
            "In one list",
            "Last updated",
            "Created",
            "Manual",
            "Branch",
            "Pull request",
            "Harness",
            "Off",
            "After 1 day",
            "After 3 days",
            "After 7 days",
        ];
        let icons = [
            icons::LAPTOP,
            icons::LIST,
            icons::CLOCK_CIRCLE,
            icons::CALENDAR,
            icons::SORT_VERTICAL,
            icons::GIT_BRANCH,
            icons::PULL_REQUEST,
            icons::BOT,
            icons::CLOSE_CIRCLE,
            icons::CLOCK_CIRCLE,
            icons::CLOCK_CIRCLE,
            icons::CLOCK_CIRCLE,
        ];
        let selected = [
            organization == SidebarOrganization::ByDevice,
            organization == SidebarOrganization::InOneList,
            sort == SidebarSort::LastUpdated,
            sort == SidebarSort::Created,
            sort == SidebarSort::Manual,
            show_branch,
            show_pr,
            show_harness,
            auto_settle_after_days.is_none(),
            auto_settle_after_days == Some(1),
            auto_settle_after_days == Some(3),
            auto_settle_after_days == Some(7),
        ];
        let mut rows: Vec<AnyElement> = SIDEBAR_VIEW_ROWS
            .iter()
            .copied()
            .enumerate()
            .map(|(ix, row)| {
                popover::menu_row_nav(
                    theme,
                    selected[ix],
                    active == Some(ix),
                    format!("sidebar-view-row-{ix}"),
                )
                .id(("sidebar-view-row", ix))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(menu) = this.sidebar_view_menu.open_mut() {
                        menu.active = None;
                    }
                    this.activate_sidebar_view_row(row, cx)
                }))
                .child(
                    icon(icons[ix])
                        .size(px(15.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(div().flex_1().child(SharedString::from(labels[ix])))
                .child(div().w(px(14.0)).flex_none().when(selected[ix], |el| {
                    el.child(
                        icon(icons::CHECK)
                            .size(px(14.0))
                            .text_color(theme.text_muted),
                    )
                }))
                .into_any_element()
            })
            .collect();
        let settle_rows = rows.split_off(8);
        let show_rows = rows.split_off(5);
        let sort_rows = rows.split_off(2);
        let organization_rows = rows;

        popover::popover_card(theme)
            .w(px(self.settings.sidebar_width - 2.0 * Theme::SPACE_SM))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.sidebar_view_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_sidebar_view_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::menu_heading(theme, "Organize"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .children(organization_rows),
            )
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Sort"))
            .child(div().flex().flex_col().gap(px(2.0)).children(sort_rows))
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Show"))
            .child(div().flex().flex_col().gap(px(2.0)).children(show_rows))
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Auto-settle inactive tasks"))
            .child(div().flex().flex_col().gap(px(2.0)).children(settle_rows))
            .into_any_element()
    }

    /// The sidebar's space-filter row: current filter ("All projects" or the
    /// space's name) + chevron, the dropdown floating beneath while open.
    /// Sits OUTSIDE the sidebar's scroll region so the float never clips.
    pub(super) fn render_spaces_filter(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.activity.view_active {
            return div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(Theme::SPACE_SM))
                .pt(px(8.0))
                .pb(px(4.0))
                .child(
                    div()
                        .id("sidebar-activity-label")
                        .flex_1()
                        .h(px(29.0))
                        .px(px(Theme::SPACE_SM))
                        .flex()
                        .items_center()
                        .text_size(crate::typography::ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child("Activity"),
                )
                .child(self.render_activity_action(theme, cx))
                .into_any_element();
        }
        let filter = self.settings.space_filter.clone();
        // Name + the dropdown rows' "@ device" tag on the trigger itself, so
        // the filtered space's host reads without opening the picker.
        let (label, device_tag): (SharedString, Option<(SharedString, bool)>) = {
            let state = self.state.read(cx);
            match filter.as_deref().and_then(|id| state.space_row(id)) {
                Some(space) => {
                    let (tag, offline) = state.space_device_tag(space, Utc::now());
                    (
                        space.display_name().to_string().into(),
                        Some((tag.into(), offline)),
                    )
                }
                None => (SharedString::from("All projects"), None),
            }
        };
        let open = self.spaces_menu.is_open();

        let trigger = div()
            .id("spaces-filter")
            .flex_1()
            .min_w_0()
            .h(px(29.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .rounded(px(8.0))
            .px(px(Theme::SPACE_SM))
            .text_size(crate::typography::ui_rems(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(motion::hover_blend(
                "spaces-filter",
                theme.text.opacity(0.8),
                theme.text,
            ))
            .bg(if open {
                theme.glass_hover()
            } else {
                motion::hover_blend(
                    "spaces-filter",
                    theme.glass_hover().opacity(0.0),
                    theme.glass_hover(),
                )
            })
            .on_hover(motion::hover_listener("spaces-filter"))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.spaces_menu.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                // A press that found the menu open closes it (the card's
                // mouse-down-out already began the close) — never reopen.
                if this.spaces_menu.take_press_was_open() {
                    this.close_spaces_menu(cx);
                } else {
                    this.open_spaces_menu(window, cx);
                }
            }))
            .child(
                icon(icons::FOLDER)
                    .size(px(16.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            // flex_1 pushes the caret to the trigger's right edge and gives
            // long space names a bound to fade against; the "@ device"
            // tag hugs the name inside it rather than sitting by the caret.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(super::sidebar_faded_label(
                        "spaces-filter-label".into(),
                        false,
                        label,
                    ))
                    .when_some(device_tag, |el, (tag, offline)| {
                        el.child(super::sidebar_faded_label(
                            "spaces-filter-device".into(),
                            false,
                            div()
                                .text_size(crate::typography::ui_rems(10.0))
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(theme.readable_muted(0.45))
                                .child(tag),
                        ))
                        // Disconnected glyph, not the word (user request).
                        .when(offline, |el| {
                            el.child(
                                icon(icons::WIFI_OFF)
                                    .size(px(12.0))
                                    .flex_none()
                                    .text_color(theme.readable_tone(theme.warning, 0.8)),
                            )
                        })
                    }),
            )
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.readable_muted(0.6)),
            );
        let trigger = if self.spaces_menu.get().is_some() {
            let closing = self.spaces_menu.closing_since();
            let menu = self.render_spaces_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below(
                "spaces-filter-menu",
                menu,
                closing,
            ))
        } else {
            trigger
        };

        let view_open = self.sidebar_view_menu.is_open();
        let view_focus = self.sidebar_view_trigger_focus.clone();
        let view_trigger = div()
            .id("sidebar-view-options")
            .role(gpui::Role::Button)
            .aria_label("Sidebar view options")
            .aria_expanded(view_open)
            .track_focus(&view_focus)
            .size(px(29.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border.opacity(0.0))
            .in_focus(|el| el.border_color(theme.border_strong))
            .cursor_pointer()
            .text_color(theme.text_muted)
            .bg(if view_open {
                theme.glass_hover()
            } else {
                theme.glass_hover().opacity(0.0)
            })
            .hover(|el| el.bg(theme.glass_hover()))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.sidebar_view_menu.note_trigger_press()),
            )
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                // Enter/Space arrive as GPUI's key-up keyboard click, which
                // has no mouse-down press note: toggle on the open state.
                let close = if event.is_keyboard() {
                    this.sidebar_view_menu.is_open()
                } else {
                    this.sidebar_view_menu.take_press_was_open()
                };
                if close {
                    this.close_sidebar_view_menu(cx);
                } else {
                    this.open_sidebar_view_menu(window, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.to_ascii_lowercase().as_str() {
                    "enter" | "space" => cx.stop_propagation(),
                    "arrowdown" => {
                        cx.stop_propagation();
                        if !this.sidebar_view_menu.is_open() {
                            this.open_sidebar_view_menu(window, cx);
                        }
                    }
                    _ => {}
                }
            }))
            .tooltip(|_, cx| cx.new(|_| SidebarViewOptionsTooltip).into())
            .tooltip_show_delay(std::time::Duration::from_millis(350))
            .child(
                icon(icons::SORT)
                    .size(px(16.0))
                    .text_color(theme.text_muted),
            );
        let view_trigger = if self.sidebar_view_menu.get().is_some() {
            let closing = self.sidebar_view_menu.closing_since();
            let menu = self.render_sidebar_view_menu(theme, cx);
            view_trigger
                .relative()
                .child(popover::anchored_menu_below_end(
                    "sidebar-view-options-menu",
                    menu,
                    closing,
                ))
        } else {
            view_trigger
        };

        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(Theme::SPACE_SM))
            .pt(px(8.0))
            .pb(px(4.0))
            .child(trigger)
            .child(view_trigger)
            .child(self.render_activity_action(theme, cx))
            .into_any_element()
    }

    /// The dropdown card: search on top, "All projects" + space rows (check on
    /// the active filter; right-click for rename/remove) + "New project…".
    fn render_spaces_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll) = {
            let Some(menu) = self.spaces_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
            )
        };
        let rows = self.spaces_menu_rows(cx);
        let scrollbar = popover::rail(self, "spaces-menu-scrollbar", theme, cx);
        let filter = self.settings.space_filter.clone();
        // Keep the host tag so projects with the same name on different
        // devices remain distinguishable. Consume `rows` to avoid cloning
        // the list children per frame.
        let details: Vec<(
            SpacesMenuRow,
            SharedString,
            Option<SharedString>,
            bool,
            bool,
        )> = {
            let state = self.state.read(cx);
            rows.into_iter()
                .map(|row| match row {
                    SpacesMenuRow::All => (
                        SpacesMenuRow::All,
                        SharedString::from("All projects"),
                        None,
                        false,
                        filter.is_none(),
                    ),
                    SpacesMenuRow::Space(id) => {
                        let selected = filter.as_deref() == Some(id.as_str());
                        match state.space_row(&id) {
                            Some(space) => {
                                let (tag, offline) = state.space_device_tag(space, Utc::now());
                                (
                                    SpacesMenuRow::Space(id),
                                    space.display_name().to_string().into(),
                                    Some(tag.into()),
                                    offline,
                                    selected,
                                )
                            }
                            None => (
                                SpacesMenuRow::Space(id),
                                SharedString::from("?"),
                                None,
                                false,
                                selected,
                            ),
                        }
                    }
                    // spaces_menu_rows never yields this variant — the
                    // footer is rendered by the card, not the list.
                    SpacesMenuRow::AddSpace => unreachable!(),
                })
                .collect()
        };
        // The pinned footer's keyboard-nav index: one past the last
        // scrollable row, its permanent place at the end of the nav order.
        let add_index = details.len();

        let list = popover::menu_scroll_host("spaces-menu-list-host")
            .on_hover(cx.listener(Self::on_spaces_menu_list_hover))
            .child(
                popover::menu_scroll_list("spaces-menu-list", &list_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    // Same scroll budget as the composer project menu.
                    .max_h(px(224.0))
                    .children(details.into_iter().enumerate().map(
                        |(ix, (row, label, tag, offline, selected))| {
                            let menu_space = match &row {
                                SpacesMenuRow::Space(id) => Some(id.clone()),
                                _ => None,
                            };
                            let activate = row;
                            popover::menu_row_nav(
                                theme,
                                selected,
                                ix == active,
                                format!("spaces-menu-row-{ix}"),
                            )
                            .id(("spaces-menu-row", ix))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_spaces_menu_row(activate.clone(), cx);
                            }))
                            .when_some(menu_space, |el, space_id| {
                                el.on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        this.space_menu.open((space_id.clone(), event.position));
                                        cx.notify();
                                    }),
                                )
                            })
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .when_some(tag, |el, tag| {
                                el.child(
                                    div()
                                        .max_w(gpui::relative(0.5))
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .font_weight(gpui::FontWeight::NORMAL)
                                        .text_color(theme.text_muted)
                                        .child(tag),
                                )
                            })
                            // Disconnected glyph, not the word (user request).
                            .when(offline, |el| {
                                el.child(
                                    icon(icons::WIFI_OFF)
                                        .size(px(12.0))
                                        .flex_none()
                                        .text_color(theme.readable_tone(theme.warning, 0.8)),
                                )
                            })
                            // No check glyph — the selected row's wash (menu_row's
                            // active styling) is the selection signal.
                        },
                    )),
            )
            .children(scrollbar);

        popover::popover_card(theme)
            // Match the trigger row as the sidebar is resized. Both live
            // inside the same SPACE_SM horizontal gutters.
            .w(px(self.settings.sidebar_width - 2.0 * Theme::SPACE_SM))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.spaces_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_spaces_menu(cx);
            }))
            .flex()
            .flex_col()
            // Same 2px rhythm as the composer project menu's root.
            .gap(px(2.0))
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            // "New project…" is a pinned action row under the list (the
            // chat composer's project menu treatment) — scrolling must
            // never carry it away, and its nav index (`add_index`) keeps it
            // LAST.
            .child(
                // Full-bleed through the card's 4px inset — a divider
                // stopping short of the edges reads as a mistake (the
                // composer project menu's treatment).
                div()
                    .my(px(2.0))
                    .mx(px(-popover::CARD_INSET))
                    .h(px(1.0))
                    .flex_none()
                    .bg(theme.border.opacity(0.6)),
            )
            .child(
                popover::menu_row_nav(
                    theme,
                    false,
                    active == add_index,
                    "spaces-menu-add".to_string(),
                )
                .id("spaces-menu-add")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.activate_spaces_menu_row(SpacesMenuRow::AddSpace, cx);
                }))
                .child(
                    icon(icons::PLUS)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from("New project…")),
                ),
            )
            .into_any_element()
    }

    /// Flat top-to-bottom chat ids exactly as [`Self::render_active_rows`]
    /// draws them — the user's sort, device grouping, and local-device
    /// promotion applied. The jump shortcuts and session cycling read THIS
    /// order (not the raw recency list) so keyboard order never drifts from
    /// the screen.
    /// Drag positions for [`SidebarSort::Manual`]; empty under other sorts.
    pub(super) fn sidebar_manual_ranks(&self) -> ManualRanks {
        if self.settings.sidebar_sort == SidebarSort::Manual {
            manual_ranks(&self.settings.sidebar_manual_order)
        } else {
            ManualRanks::new()
        }
    }

    pub(super) fn sidebar_visible_order(&self, cx: &Context<Self>) -> Vec<String> {
        let filter = self.settings.space_filter.clone();
        let attention_task_keys = self.activity_task_keys(cx);
        let recent_completion_keys = self.activity_recent_completion_keys();
        let mut settlement_task_keys = attention_task_keys.clone();
        settlement_task_keys.extend(self.settlement_blocking_task_keys());
        let state = self.state.read(cx);
        let now = Utc::now();
        let selected = state.selected_chat.as_deref();
        let mut chats: Vec<luna_proto::Chat> = if self.activity.view_active {
            fold_child_chats(
                activity_sidebar_chats_with_recent(
                    state.sidebar_chats(now, None),
                    &attention_task_keys,
                    &recent_completion_keys,
                ),
                selected,
            )
            .into_iter()
            .filter(|(_, chat)| !self.pending_task_actions.contains_key(&chat.id))
            .map(|(_, chat)| chat.clone())
            .collect()
        } else {
            let mut chats = fold_child_chats(state.sidebar_chats(now, filter.as_deref()), selected);
            let ranks = self.sidebar_manual_ranks();
            chats.sort_by(|left, right| {
                compare_sidebar_chats_ranked(self.settings.sidebar_sort, &ranks, left.1, right.1)
            });
            let available = chats.clone();
            let selected = active_page(
                chats,
                state.selected_chat.as_deref(),
                now,
                self.settings.sidebar_auto_settle_after_days,
                &settlement_task_keys,
                &self.pending_task_actions,
                self.active_shown,
            )
            .0;
            include_child_families(selected, available)
                .into_iter()
                .map(|(_, chat)| chat.clone())
                .collect()
        };
        if self.activity.view_active
            || self.settings.sidebar_organization != SidebarOrganization::ByDevice
        {
            return child_visible_tree(&chats)
                .into_iter()
                .map(|row| row.id)
                .collect();
        }
        let pinned_len = chats
            .iter()
            .take_while(|chat| chat.pinned_order.is_some())
            .count();
        let unpinned = chats.split_off(pinned_len);
        let mut ordered: Vec<String> = chats.into_iter().map(|chat| chat.id).collect();
        let mut groups: Vec<(Option<(String, String)>, Vec<luna_proto::Chat>)> = Vec::new();
        for chat in unpinned {
            let key = Some((chat.device_id.clone(), String::new()));
            if let Some((_, existing)) = groups.iter_mut().find(|(group, _)| group == &key) {
                existing.push(chat);
            } else {
                groups.push((key, vec![chat]));
            }
        }
        promote_local_device_group(&mut groups, state.local_device_id.as_deref());
        ordered.extend(
            groups
                .into_iter()
                .flat_map(|(_, rows)| rows)
                .map(|chat| chat.id),
        );
        let chats: Vec<_> = ordered
            .iter()
            .filter_map(|id| state.chats.iter().find(|chat| &chat.id == id).cloned())
            .collect();
        child_visible_tree(&chats)
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    /// Normal sidebar: every session (idle included) under the saved project
    /// filter and sort. Activity view: only actionable/running sessions from
    /// all projects, using the same row renderer. Rows are keyed for FLIP.
    pub(super) fn render_active_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<(String, f32, AnyElement)> {
        let now = Utc::now();
        let filter = self.settings.space_filter.clone();
        let activity_view = self.activity.view_active;
        let attention_task_keys = self.activity_task_keys(cx);
        let recent_completion_keys = self.activity_recent_completion_keys();
        let mut settlement_task_keys = attention_task_keys.clone();
        settlement_task_keys.extend(self.settlement_blocking_task_keys());
        let mut rows: Vec<ActiveChatRow> = {
            let state = self.state.read(cx);
            let selected = state.selected_chat.as_deref();
            let (chats, hidden_count): (Vec<_>, usize) = if activity_view {
                (
                    fold_child_chats(
                        activity_sidebar_chats_with_recent(
                            state.sidebar_chats(now, None),
                            &attention_task_keys,
                            &recent_completion_keys,
                        ),
                        selected,
                    ),
                    0,
                )
            } else {
                let mut chats =
                    fold_child_chats(state.sidebar_chats(now, filter.as_deref()), selected);
                let ranks = self.sidebar_manual_ranks();
                chats.sort_by(|left, right| {
                    compare_sidebar_chats_ranked(
                        self.settings.sidebar_sort,
                        &ranks,
                        left.1,
                        right.1,
                    )
                });
                let available = chats.clone();
                let (selected, hidden) = active_page(
                    chats,
                    state.selected_chat.as_deref(),
                    now,
                    self.settings.sidebar_auto_settle_after_days,
                    &settlement_task_keys,
                    &self.pending_task_actions,
                    self.active_shown,
                );
                (include_child_families(selected, available), hidden)
            };
            self.sidebar_hidden_active_count = hidden_count;
            chats
                .into_iter()
                .filter(|(_, chat)| {
                    !activity_view || !self.pending_task_actions.contains_key(&chat.id)
                })
                .map(|(status, chat)| {
                    let has_attention =
                        settlement_task_keys.contains(&(chat.device_id.clone(), chat.id.clone()));
                    let has_queued_message = state.selected_chat.as_deref()
                        == Some(chat.id.as_str())
                        && !state.queue.is_empty();
                    let can_settle = !activity_view
                        && can_manually_settle_chat(
                            chat,
                            status,
                            has_attention,
                            has_queued_message,
                        );
                    let chat = chat.clone();
                    // Line 1 is the task's project and host device.
                    let project = state
                        .space_for_chat(&chat)
                        .map(|space| space.display_name().to_string())
                        .unwrap_or_else(|| "Unknown project".to_string());
                    let device = state
                        .device_name(&chat.device_id)
                        .unwrap_or("Unknown device")
                        .to_string();
                    let folder = project;
                    // The branch shows whenever the engine has stamped one —
                    // main-checkout sessions included, not just worktrees.
                    let branch = crate::change_requests::conversation_branch(&chat, &state.spaces)
                        .map(str::trim)
                        .filter(|b| !b.is_empty())
                        .map(str::to_string);
                    let change_request = state.change_request_for_chat(&chat).cloned();
                    let group = if activity_view {
                        None
                    } else if chat.pinned_order.is_some() {
                        Some(("__luna_pinned__".into(), "Pinned".into()))
                    } else {
                        match self.settings.sidebar_organization {
                            SidebarOrganization::ByDevice => Some((chat.device_id.clone(), device)),
                            SidebarOrganization::ByProject | SidebarOrganization::InOneList => None,
                        }
                    };
                    ActiveChatRow {
                        depth: 0,
                        status,
                        chat: chat.clone(),
                        can_settle,
                        folder,
                        branch,
                        change_request,
                        group,
                    }
                })
                .collect()
        };
        if !self.settings.sidebar_show_branch {
            for row in &mut rows {
                row.branch = None;
            }
        }
        if !self.settings.sidebar_show_pull_request {
            for row in &mut rows {
                row.change_request = None;
            }
        }

        let hierarchy_chats: Vec<_> = rows.iter().map(|row| row.chat.clone()).collect();
        let hierarchy = child_visible_tree(&hierarchy_chats);
        let mut mapped: std::collections::HashMap<String, ActiveChatRow> = rows
            .into_iter()
            .map(|row| (row.chat.id.clone(), row))
            .collect();
        rows = hierarchy
            .into_iter()
            .filter_map(|node| {
                mapped.remove(&node.id).map(|mut row| {
                    row.depth = node.depth;
                    row
                })
            })
            .collect();
        let child_counts = super::child_chats::direct_child_counts(&self.state.read(cx).chats);

        let mut groups: Vec<(Option<(String, String)>, Vec<ActiveChatRow>)> = Vec::new();
        for row in rows {
            if let Some((_, existing)) = groups.iter_mut().find(|(group, _)| group == &row.group) {
                existing.push(row);
            } else {
                groups.push((row.group.clone(), vec![row]));
            }
        }
        if !activity_view && self.settings.sidebar_organization == SidebarOrganization::ByDevice {
            let local_device_id = self.state.read(cx).local_device_id.clone();
            if groups.first().is_some_and(|(group, _)| {
                group
                    .as_ref()
                    .is_some_and(|(id, _)| id == "__luna_pinned__")
            }) {
                let pinned = groups.remove(0);
                promote_local_device_group(&mut groups, local_device_id.as_deref());
                groups.insert(0, pinned);
            } else {
                promote_local_device_group(&mut groups, local_device_id.as_deref());
            }
        }

        let selected = self.state.read(cx).selected_chat.clone();
        // Re-checked at render so the chips drop the FRAME a popover opens,
        // not on the next modifier event — the jumps are suppressed under it.
        let jump_hints = self.jump_hints && !self.overlay_owns_keyboard(cx);
        let keymap = self.settings.keymap.clone();
        // Flat top-to-bottom slot across groups: the same order
        // `sidebar_visible_order` hands the jump shortcuts and cycling, so a
        // chip always names the key that opens its row.
        let mut slot = 0usize;
        // A drop released outside every row ends GPUI's drag without our
        // `on_drop`; never leave the source row dimmed with a stale slot.
        if self.sidebar_drag.is_some() && !cx.has_active_drag() {
            self.sidebar_drag = None;
        }
        let organization = self.settings.sidebar_organization;
        let mut drag_rows: Vec<(String, SidebarDragScope)> = Vec::new();
        let mut drag_root: Option<String> = None;
        let drop_slot = self
            .sidebar_drag
            .as_ref()
            .and_then(|state| state.target.clone());
        let has_unpinned_rows = groups.iter().any(|(group, _)| {
            !group
                .as_ref()
                .is_some_and(|(key, _)| key == "__luna_pinned__")
        });
        let mut rendered = Vec::new();
        for (group, rows) in groups {
            let mut rendered_rows = Vec::with_capacity(rows.len());
            for row in rows {
                let ActiveChatRow {
                    depth,
                    status,
                    chat,
                    can_settle,
                    folder,
                    branch,
                    change_request,
                    group: _,
                } = row;
                let time_ago: SharedString =
                    format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now).into();
                let is_selected = selected.as_deref() == Some(chat.id.as_str());
                let title: SharedString = transcript::single_line(
                    &chat.title.clone().unwrap_or_else(|| "New session".into()),
                )
                .into();
                let device: SharedString = self
                    .state
                    .read(cx)
                    .device_name(&chat.device_id)
                    .unwrap_or("Unknown device")
                    .to_string()
                    .into();
                let details = sidebar_thread_tooltip(
                    &chat,
                    title.clone(),
                    folder.clone().into(),
                    device,
                    branch.clone().map(SharedString::from),
                    (status == ChatIndicator::Completed).then(|| "Done".into()),
                );
                let harness = self
                    .settings
                    .sidebar_show_harness
                    .then(|| chat.config.as_ref().map(|c| c.harness))
                    .flatten();
                let height = super::chat_row_height(branch.is_some(), change_request.is_some());
                // Only rows a jump slot can reach wear a chip; row 10 onward
                // keeps its time-ago.
                let jump_label: Option<SharedString> = if jump_hints {
                    let combo = keymap.get(ShortcutId::JumpSession(slot));
                    (slot < JUMP_SLOTS && !combo.is_empty()).then(|| badge_combo(combo).into())
                } else {
                    None
                };
                slot += 1;
                // Only root rows reorder. The selected chat's nested lineage
                // drops after its root, where the tree keeps it anyway.
                let scope = (!activity_view).then(|| {
                    if chat.pinned_order.is_some() {
                        SidebarDragScope::Pinned
                    } else {
                        SidebarDragScope::Unpinned(
                            (organization == SidebarOrganization::ByDevice)
                                .then(|| chat.device_id.clone()),
                        )
                    }
                });
                if depth == 0 {
                    drag_root = Some(chat.id.clone());
                    if let Some(scope) = &scope {
                        drag_rows.push((chat.id.clone(), scope.clone()));
                    }
                }
                let drag = scope.filter(|_| depth == 0).map(|scope| SidebarChatDrag {
                    chat_id: chat.id.clone(),
                    scope,
                    title: title.clone(),
                    project: folder.clone().into(),
                    harness: chat.config.as_ref().map(|config| config.harness),
                });
                let element = self.render_chat_row(
                    ChatRowProps {
                        id: chat.id.clone(),
                        title,
                        time_ago,
                        space_name: folder.into(),
                        branch: branch.map(SharedString::from),
                        note: None,
                        change_request,
                        harness,
                        status,
                        status_label: None,
                        selected: is_selected,
                        archived: false,
                        pinned: chat.pinned_order.is_some(),
                        surface: if activity_view {
                            ChatRowSurface::Activity
                        } else {
                            ChatRowSurface::Sidebar
                        },
                        can_settle,
                        settled: false,
                        child_chats: child_counts.get(&chat.id).copied().unwrap_or(0),
                        jump_label,
                        search_query: None,
                        details: Some(details),
                        drag,
                    },
                    theme,
                    cx,
                );
                let slot_place = drop_slot
                    .as_ref()
                    .filter(|(anchor, _)| depth == 0 && *anchor == chat.id)
                    .map(|(_, place)| *place);
                let target_row = if depth == 0 {
                    Some(chat.id.clone())
                } else {
                    drag_root.clone()
                };
                // Only the selected chat's lineage is nested now; the rest of
                // a parent's children live in its right-pane Child chats list.
                let element = div()
                    .id(SharedString::from(format!("sidebar-row-{}", chat.id)))
                    .relative()
                    .pl(px((depth.min(4) * 14) as f32))
                    .child(element)
                    .when_some(target_row.filter(|_| !activity_view), |row, target| {
                        let nested = depth > 0;
                        row.on_drag_move::<SidebarChatDrag>(cx.listener(
                            move |this, event: &gpui::DragMoveEvent<SidebarChatDrag>, _, cx| {
                                if !event.bounds.contains(&event.event.position) {
                                    return;
                                }
                                let lower =
                                    nested || event.event.position.y > event.bounds.center().y;
                                let drag = event.drag(cx).clone();
                                this.update_sidebar_drag(&drag, &target, lower, cx);
                            },
                        ))
                        .on_drop::<SidebarChatDrag>(cx.listener(
                            |this, drag: &SidebarChatDrag, _, cx| {
                                this.finish_sidebar_drag(drag, cx);
                            },
                        ))
                    })
                    .when_some(slot_place, |row, place| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(Theme::SPACE_SM))
                                .right(px(Theme::SPACE_SM))
                                .h(px(2.0))
                                .rounded_full()
                                .bg(theme.accent)
                                .map(|line| match place {
                                    SidebarDropPlace::Before => line.top(px(-1.0)),
                                    SidebarDropPlace::After => line.bottom(px(-1.0)),
                                }),
                        )
                    })
                    .into_any_element();
                rendered_rows.push((format!("c:{}", chat.id), height, element));
            }

            let Some((key, label)) = group else {
                rendered.extend(rendered_rows);
                continue;
            };
            let organization = if key == "__luna_pinned__" {
                "pinned"
            } else {
                match self.settings.sidebar_organization {
                    SidebarOrganization::ByDevice => "device",
                    SidebarOrganization::ByProject | SidebarOrganization::InOneList => "list",
                }
            };
            let collapse_key = format!("{organization}:{key}");
            let motion_key = format!("group:{collapse_key}");
            let collapsed = self.sidebar_collapsed_groups.contains(&collapse_key);
            let row_count = rendered_rows.len();
            let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
                + rendered_rows
                    .iter()
                    .map(|(_, height, _)| *height)
                    .sum::<f32>()
                + SIDEBAR_LIST_GAP * row_count.saturating_sub(1) as f32;
            let body = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP))
                .children(rendered_rows.into_iter().map(|(_, _, row)| row));
            let visible_label: SharedString = if collapsed {
                format!("{label} ({row_count})").into()
            } else {
                label.into()
            };
            let chevron = self.sidebar_disclosure_chevron(&motion_key, !collapsed, theme);
            let toggle_key = collapse_key.clone();
            let toggle_motion_key = motion_key.clone();
            let header = sidebar_disclosure_header(theme, visible_label, chevron)
                .id(SharedString::from(format!("sidebar-group-{collapse_key}")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let was_open = !this.sidebar_collapsed_groups.contains(&toggle_key);
                    this.begin_sidebar_disclosure_motion(
                        &toggle_motion_key,
                        if was_open { body_height } else { 0.0 },
                        if was_open { 0.0 } else { body_height },
                    );
                    if was_open {
                        this.sidebar_collapsed_groups.insert(toggle_key.clone());
                    } else {
                        this.sidebar_collapsed_groups.remove(&toggle_key);
                    }
                    cx.notify();
                }));
            let body = self.render_sidebar_disclosure_body(
                &motion_key,
                !collapsed,
                body_height,
                body.into_any_element(),
            );
            let height =
                SIDEBAR_DISCLOSURE_SECTION_HEIGHT + if collapsed { 0.0 } else { body_height };
            let element = div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_SECTION_GAP))
                .child(header)
                .child(body)
                .into_any_element();
            rendered.push((format!("g:{collapse_key}"), height, element));
            if key == "__luna_pinned__" && has_unpinned_rows && !collapsed {
                let divider = div()
                    .w_full()
                    .pt(px(8.0))
                    .pb(px(8.0))
                    .px(px(Theme::SPACE_SM))
                    .child(sidebar_separator(theme))
                    .into_any_element();
                rendered.push(("divider:pinned".into(), 17.0, divider));
            }
        }
        self.sidebar_drag_rows = drag_rows;
        if self.sidebar_hidden_active_count > 0 {
            let remaining = self.sidebar_hidden_active_count.min(ACTIVE_PAGE);
            let more = div()
                .id("active-more")
                .mt(px(SIDEBAR_SECTION_GAP))
                .h(px(36.0))
                .px(px(Theme::SPACE_SM))
                .flex()
                .flex_row()
                .items_center()
                // Plus is 14px; task harness marks are 13px with an 8px gap.
                // A 7px gap puts this label on the same text column as titles.
                .gap(px(7.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(13.0))
                .text_color(theme.text_muted)
                .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_shown += ACTIVE_PAGE;
                    cx.notify();
                }))
                .child(icon(icons::PLUS).size(px(14.0)))
                .child(SharedString::from(format!("Show {remaining} more")))
                .into_any_element();
            rendered.push(("more:active".into(), 36.0 + SIDEBAR_SECTION_GAP, more));
        }
        rendered
    }

    /// Settled tasks stay in the synced task list; automatic settling is a
    /// derived view, while deliberate overrides live on the task record.
    pub(super) fn render_settled_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let mut attention_task_keys = self.activity_task_keys(cx);
        attention_task_keys.extend(self.settlement_blocking_task_keys());
        let mut rows: Vec<luna_proto::Chat> = {
            let state = self.state.read(cx);
            // Same folding as the active list, so a child never shows in
            // Settled while its parent is listed in either section.
            fold_child_chats(
                state.sidebar_chats(now, self.settings.space_filter.as_deref()),
                state.selected_chat.as_deref(),
            )
            .into_iter()
            .filter(
                |(status, chat)| match self.pending_task_actions.get(&chat.id) {
                    Some(TaskConfirmation::Settle) => true,
                    Some(TaskConfirmation::Archive | TaskConfirmation::Delete) => false,
                    None => is_settled_chat(
                        chat,
                        *status,
                        now,
                        self.settings.sidebar_auto_settle_after_days,
                        attention_task_keys.contains(&(chat.device_id.clone(), chat.id.clone())),
                    ),
                },
            )
            .map(|(_, chat)| chat.clone())
            .collect()
        };
        if rows.is_empty() {
            return None;
        }
        rows.sort_by(|left, right| compare_sidebar_chats(SidebarSort::LastUpdated, left, right));
        let total = rows.len();
        let shown = self.settled_shown.max(SETTLED_INITIAL);
        let visible_count = total.min(shown);
        let has_more = total > shown;
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + visible_count as f32 * 36.0
            + visible_count.saturating_sub(1) as f32 * SIDEBAR_LIST_GAP
            + if has_more {
                36.0 + SIDEBAR_LIST_GAP
            } else {
                0.0
            };
        let open = self.settled_open;
        let label: SharedString = if open {
            "Settled".into()
        } else {
            format!("Settled ({total})").into()
        };
        let header = sidebar_disclosure_header(
            theme,
            label,
            self.sidebar_disclosure_chevron("settled", open, theme),
        )
        .id("settled-toggle")
        .on_click(cx.listener(move |this, _, _, cx| {
            this.begin_sidebar_disclosure_motion(
                "settled",
                if this.settled_open { body_height } else { 0.0 },
                if this.settled_open { 0.0 } else { body_height },
            );
            this.settled_open = !this.settled_open;
            this.settled_shown = SETTLED_INITIAL;
            cx.notify();
        }));
        let selected = self.state.read(cx).selected_chat.clone();
        let mut list = div()
            .flex()
            .flex_col()
            .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
            .gap(px(SIDEBAR_LIST_GAP));
        for chat in rows.into_iter().take(shown) {
            let id = chat.id.clone();
            let menu_id = id.clone();
            let title = transcript::single_line(
                &chat.title.clone().unwrap_or_else(|| "New session".into()),
            );
            let time_ago = format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now);
            let selected_row = selected.as_deref() == Some(id.as_str());
            let details = {
                let state = self.state.read(cx);
                sidebar_thread_tooltip(
                    &chat,
                    title.clone().into(),
                    state
                        .space_for_chat(&chat)
                        .map(|space| space.display_name().to_string())
                        .unwrap_or_else(|| "Unknown project".into())
                        .into(),
                    state
                        .device_name(&chat.device_id)
                        .unwrap_or("Unknown device")
                        .to_string()
                        .into(),
                    chat.branch.clone().map(SharedString::from),
                    Some("Settled".into()),
                )
            };
            let tooltip_height = details.estimated_height();
            list = list.child(
                div()
                    .id(SharedString::from(format!("settled-{id}")))
                    .h(px(36.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(Theme::SPACE_SM))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .tooltip_at(
                        move |_, cx| cx.new(|_| details.clone()).into(),
                        move |bounds, pointer, window| {
                            sidebar_thread_tooltip_anchor(
                                bounds,
                                pointer,
                                window,
                                0.0,
                                tooltip_height,
                            )
                        },
                    )
                    .tooltip_show_delay(Duration::from_millis(300))
                    .when(selected_row, |el| el.bg(crate::theme::glass_selected_bg()))
                    .hover(|el| el.bg(theme.glass_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_chat(id.clone(), cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.chat_menu.open(ChatMenuState {
                                chat_id: menu_id.clone(),
                                position: event.position,
                                page: ChatMenuPage::Root,
                            });
                            cx.notify();
                        }),
                    )
                    .child(sidebar_faded_label(
                        format!("settled-title-{}", chat.id).into(),
                        true,
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text.opacity(0.86))
                            .child(SharedString::from(title)),
                    ))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_muted(0.55))
                            .child(SharedString::from(time_ago)),
                    ),
            );
        }
        let mut body = div().w_full().flex().flex_col().child(list);
        if has_more {
            let remaining = (total - shown).min(SETTLED_PAGE);
            body = body.child(
                div()
                    .id("settled-more")
                    .mt(px(SIDEBAR_LIST_GAP))
                    .h(px(36.0))
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .hover(|el| el.bg(theme.glass_hover()).text_color(theme.text))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settled_shown = this.settled_shown.max(SETTLED_INITIAL) + SETTLED_PAGE;
                        cx.notify();
                    }))
                    .child(icon(icons::PLUS).size(px(14.0)))
                    .child(SharedString::from(format!("Show {remaining} more"))),
            );
        }
        let body = self.render_sidebar_disclosure_body(
            "settled",
            open,
            body_height,
            body.into_any_element(),
        );
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_SECTION_GAP))
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    /// The sidebar's archived shelf — a direct port of t3code's settled
    /// shelf: header is label + hairline + chevron ("Archived (N)" closed,
    /// "Archived" open), rows are 36px SLIM one-liners (dimmed harness mark,
    /// title, time-ago right — the time yields to Unarchive on row hover),
    /// and the tail pages behind an explicit "Show N more" row (initial 10,
    /// +25 a click). `None` when nothing is archived under the current
    /// project filter.
    pub(super) fn render_archived_section(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        const INITIAL: usize = 10;
        const PAGE: usize = 25;
        let now = Utc::now();
        let filter = self.settings.space_filter.clone();
        let mut rows: Vec<luna_proto::Chat> = {
            let state = self.state.read(cx);
            let rows_by_id: std::collections::HashMap<&str, &luna_proto::Chat> = state
                .chats
                .iter()
                .map(|chat| (chat.id.as_str(), chat))
                .collect();
            state
                .chats
                .iter()
                .filter(|c| c.archived)
                // An archived child stays listed in its parent's Child chats
                // pane; only children whose parent is gone keep a shelf row.
                .filter(|chat| lineage_parent(&rows_by_id, chat).is_none())
                .filter(|chat| {
                    self.pending_task_actions.get(&chat.id) != Some(&TaskConfirmation::Delete)
                })
                .filter(|chat| {
                    chat.space_id
                        .as_deref()
                        .is_some_and(|id| state.space_row(id).is_some())
                })
                .filter(|chat| match &filter {
                    Some(space_id) => chat.space_id.as_deref() == Some(space_id.as_str()),
                    None => true,
                })
                .cloned()
                .collect()
        };
        // A drag order describes the active list; the shelf stays by recency.
        let sort = match self.settings.sidebar_sort {
            SidebarSort::Manual => SidebarSort::LastUpdated,
            sort => sort,
        };
        rows.sort_by(|left, right| compare_sidebar_chats(sort, left, right));
        if rows.is_empty() {
            return None;
        }
        let total = rows.len();
        let open = self.archived_open;
        let shown = self.archived_shown.max(INITIAL);
        let visible_count = total.min(shown);
        let has_more = total > shown;
        let body_height = SIDEBAR_DISCLOSURE_BODY_INSET
            + visible_count as f32 * 36.0
            + visible_count.saturating_sub(1) as f32 * SIDEBAR_LIST_GAP
            + if has_more {
                36.0 + SIDEBAR_LIST_GAP
            } else {
                0.0
            };
        // Header (t3code settled-shelf toggle): muted 12px label, a hairline
        // filling the middle, chevron flipping open/closed. The count only
        // shows while collapsed — expanded, the rows speak for themselves.
        let label: SharedString = if open {
            "Archived".into()
        } else {
            format!("Archived ({total})").into()
        };
        let chevron = self.sidebar_disclosure_chevron("archived", open, theme);
        let header = sidebar_disclosure_header(theme, label, chevron)
            .id("archived-toggle")
            .on_click(cx.listener(move |this, _, _, cx| {
                let was_open = this.archived_open;
                this.begin_sidebar_disclosure_motion(
                    "archived",
                    if was_open { body_height } else { 0.0 },
                    if was_open { 0.0 } else { body_height },
                );
                this.archived_open = !was_open;
                this.archived_shown = INITIAL;
                cx.notify();
            }));
        let section = div().flex().flex_col().child(header);
        let body = {
            let selected = self.state.read(cx).selected_chat.clone();
            let selected_wash = crate::theme::glass_selected_bg();
            let mut list = div()
                .flex()
                .flex_col()
                .pt(px(SIDEBAR_DISCLOSURE_BODY_INSET))
                .gap(px(SIDEBAR_LIST_GAP));
            for chat in rows.into_iter().take(shown) {
                let id = chat.id.clone();
                let hovered = self.archived_hover.as_deref() == Some(id.as_str());
                let is_selected = selected.as_deref() == Some(id.as_str());
                let title: SharedString = transcript::single_line(
                    &chat.title.clone().unwrap_or_else(|| "New session".into()),
                )
                .into();
                let time_ago: SharedString =
                    format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), now).into();
                let details = {
                    let state = self.state.read(cx);
                    sidebar_thread_tooltip(
                        &chat,
                        title.clone(),
                        state
                            .space_for_chat(&chat)
                            .map(|space| space.display_name().to_string())
                            .unwrap_or_else(|| "Unknown project".into())
                            .into(),
                        state
                            .device_name(&chat.device_id)
                            .unwrap_or("Unknown device")
                            .to_string()
                            .into(),
                        chat.branch.clone().map(SharedString::from),
                        Some("Archived".into()),
                    )
                };
                let brand = if self.settings.sidebar_show_harness {
                    chat.config
                        .as_ref()
                        .map(|c| crate::pickers::harness_brand_icon(c.harness))
                } else {
                    None
                };
                // Right slot: time at rest; the Unarchive affordance takes
                // its place on row hover (t3code: "only the time/jump label
                // yields to the settle affordance").
                let right: AnyElement = if hovered {
                    let restore_id = id.clone();
                    div()
                        .id(SharedString::from(format!("archived-restore-{id}")))
                        .role(gpui::Role::Button)
                        .aria_label("Unarchive task")
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.0))
                        .rounded(px(4.0))
                        .hover(|s| s.bg(theme.glass_hover()))
                        .tooltip(|_, cx| {
                            cx.new(|_| SurfaceTabTooltip {
                                text: "Unarchive task".into(),
                            })
                            .into()
                        })
                        .tooltip_show_delay(Duration::from_millis(350))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.set_chat_archived(restore_id.clone(), false, cx);
                        }))
                        .child(
                            crate::icons::icon(crate::icons::ARCHIVE_UP_MINIMALISTIC)
                                .size(px(13.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .into_any_element()
                } else {
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(if theme.appearance.is_light() {
                            theme.text_muted
                        } else {
                            theme.text_muted.opacity(0.55)
                        })
                        .child(time_ago)
                        .into_any_element()
                };
                let hover_id = id.clone();
                let open_id = id.clone();
                let menu_id = id.clone();
                let tooltip_height = details.estimated_height();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("archived-{id}")))
                        .h(px(36.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SIDEBAR_ARCHIVED_HARNESS_TITLE_GAP))
                        .px(px(Theme::SPACE_SM))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .tooltip_at(
                            move |_, cx| cx.new(|_| details.clone()).into(),
                            move |bounds, pointer, window| {
                                sidebar_thread_tooltip_anchor(
                                    bounds,
                                    pointer,
                                    window,
                                    28.0,
                                    tooltip_height,
                                )
                            },
                        )
                        .tooltip_show_delay(Duration::from_millis(300))
                        .when(is_selected, |el| el.bg(selected_wash))
                        .when(!is_selected, |el| el.hover(|s| s.bg(theme.glass_hover())))
                        .on_hover(cx.listener(move |this, entered: &bool, _, cx| {
                            if *entered {
                                if this.archived_hover.as_deref() != Some(hover_id.as_str()) {
                                    this.archived_hover = Some(hover_id.clone());
                                    cx.notify();
                                }
                            } else if this.archived_hover.as_deref() == Some(hover_id.as_str()) {
                                this.archived_hover = None;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_chat(open_id.clone(), cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                this.chat_menu.open(ChatMenuState {
                                    chat_id: menu_id.clone(),
                                    position: event.position,
                                    page: ChatMenuPage::Root,
                                });
                                cx.notify();
                            }),
                        )
                        // Archived history recedes: dimmed mark at rest,
                        // restored on hover (t3code's grayscale favicon).
                        .when_some(brand, |el, (mark, tint)| {
                            el.child(
                                crate::icons::icon(mark)
                                    .size(px(SIDEBAR_ARCHIVED_HARNESS_ICON_SIZE))
                                    .flex_none()
                                    .text_color(if hovered || is_selected {
                                        tint.unwrap_or(theme.text_muted)
                                    } else {
                                        tint.unwrap_or(theme.text_muted).opacity(0.4)
                                    }),
                            )
                        })
                        .child(super::sidebar_faded_label(
                            format!("archived-title-{id}").into(),
                            true,
                            div()
                                .text_size(crate::typography::ui_rems(13.0))
                                .text_color(if hovered || is_selected {
                                    theme.text
                                } else {
                                    theme.text.opacity(0.55)
                                })
                                .child(title),
                        ))
                        .child(right),
                );
            }
            let mut body = div().w_full().flex().flex_col().child(list);
            if has_more {
                let remaining = (total - shown).min(PAGE);
                body = body.child(
                    div()
                        .id("archived-more")
                        // Sits outside the rows' gapped column — match the
                        // list's 2px row gap or it fuses with the last row.
                        .mt(px(2.0))
                        .h(px(36.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(Theme::SPACE_SM))
                        .rounded(px(6.0))
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.readable_muted(0.55))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.archived_shown = this.archived_shown.max(INITIAL) + PAGE;
                            cx.notify();
                        }))
                        .child(
                            crate::icons::icon(crate::icons::PLUS)
                                .size(px(14.0))
                                .flex_none(),
                        )
                        .child(SharedString::from(format!("Show {remaining} more"))),
                );
            }
            body.into_any_element()
        };
        let body = self.render_sidebar_disclosure_body("archived", open, body_height, body);
        let section = section.pt(px(SIDEBAR_SECTION_GAP)).child(body);
        Some(section.into_any_element())
    }

    // ---- add-space flow ----

    pub(super) fn open_add_space(&mut self, cx: &mut Context<Self>) {
        self.open_add_space_with_origin(AddSpaceOrigin::Standalone, cx);
    }

    pub(super) fn open_add_space_for_move(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.open_add_space_with_origin(AddSpaceOrigin::MoveTask { chat_id }, cx);
    }

    fn open_add_space_with_origin(&mut self, origin: AddSpaceOrigin, cx: &mut Context<Self>) {
        self.dismiss_command_palette(cx);
        self.add_space_flow_seq = self.add_space_flow_seq.wrapping_add(1);
        let flow_id = self.add_space_flow_seq;
        // "PaletteSearch" context: navigation keys stay unbound so ↑↓/←/→/⏎
        // bubble to the palette frame (`add_space_key`) instead of moving the
        // text caret — Enter and ⌘Enter are both handled there.
        let search =
            cx.new(|cx| ComposerInput::with_context("Search devices…", "PaletteSearch", cx));
        let search_events = cx.subscribe(&search, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(flow) = this.add_space.as_mut() {
                    if !flow.search.read(cx).is_empty() {
                        flow.pending_home_path = None;
                    }
                    flow.active = 0;
                    flow.list_scroll.set_offset(gpui::Point::default());
                }
                cx.notify();
            }
        });
        self.add_space = Some(AddSpaceFlow {
            id: flow_id,
            origin,
            step: ProjectStep::Devices,
            location: None,
            device: None,
            search,
            browser: Loadable::Idle,
            drives: Loadable::Idle,
            browser_path: None,
            home: None,
            browser_repo: false,
            active: 0,
            submit_busy: false,
            error: None,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            focus_pending: true,
            load_task: None,
            drives_task: None,
            submit_task: None,
            folder_request: 0,
            drives_request: 0,
            pending_home_path: None,
            _search_events: search_events,
        });
        // A standalone project normally belongs to this Mac. Start at its
        // Home folder, with the device breadcrumb still available to switch.
        if matches!(
            self.add_space.as_ref().map(|f| &f.origin),
            Some(AddSpaceOrigin::Standalone)
        ) {
            let state = self.state.read(cx);
            let local = state
                .local_device_id
                .as_deref()
                .and_then(|id| state.devices.iter().find(|device| device.id == id).cloned());
            if let Some(local) = local {
                self.add_space_pick_device(local, cx);
                self.add_space_goto_location("Home".into(), None, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn dismiss_add_space(&mut self, cx: &mut Context<Self>) {
        self.add_space = None;
        cx.notify();
    }

    /// Selecting a device advances to its locations.
    fn add_space_pick_device(&mut self, device: Device, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = ProjectStep::Locations;
        flow.location = None;
        flow.load_task = None;
        flow.drives_task = None;
        flow.folder_request = flow.folder_request.wrapping_add(1);
        flow.drives_request = flow.drives_request.wrapping_add(1);
        flow.list_scroll.set_offset(gpui::Point::default());
        flow.device = Some(device);
        flow.browser = Loadable::Idle;
        flow.drives = Loadable::Idle;
        flow.browser_path = None;
        flow.home = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        flow.pending_home_path = None;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search locations or paste a path…", cx);
            input.set_text("", cx);
        });
        self.load_space_drives(cx);
        cx.notify();
    }

    fn add_space_goto_location(
        &mut self,
        name: String,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = ProjectStep::Folders;
        flow.location = Some((name, path.clone()));
        flow.browser_repo = false;
        flow.pending_home_path = None;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders or paste a path…", cx);
            input.set_text("", cx);
        });
        self.load_space_folders(path, cx);
    }

    fn add_space_back_to(&mut self, step: ProjectStep, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        flow.step = step;
        flow.load_task = None;
        flow.folder_request = flow.folder_request.wrapping_add(1);
        flow.browser = Loadable::Idle;
        flow.browser_path = None;
        flow.location = None;
        flow.browser_repo = false;
        flow.active = 0;
        flow.error = None;
        flow.pending_home_path = None;
        flow.list_scroll.set_offset(gpui::Point::default());
        if step == ProjectStep::Devices {
            flow.drives_task = None;
            flow.drives_request = flow.drives_request.wrapping_add(1);
            flow.device = None;
            flow.drives = Loadable::Idle;
            flow.home = None;
        }
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder(
                if step == ProjectStep::Devices {
                    "Search devices…"
                } else {
                    "Search locations or paste a path…"
                },
                cx,
            );
            input.set_text("", cx);
        });
        cx.notify();
    }

    fn add_space_devices(&self, cx: &App) -> Vec<Device> {
        let Some(flow) = &self.add_space else {
            return Vec::new();
        };
        let devices = &self.state.read(cx).devices;
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        popover::filter_indices(flow.search.read(cx).text(), &names)
            .into_iter()
            .map(|ix| devices[ix].clone())
            .collect()
    }

    fn add_space_locations(&self, cx: &App) -> Vec<(String, Option<String>)> {
        let Some(flow) = &self.add_space else {
            return Vec::new();
        };
        let locations: Vec<_> = std::iter::once(("Home".to_string(), None))
            .chain(
                flow.drives
                    .ready()
                    .into_iter()
                    .flatten()
                    .map(|d| (d.name.clone(), Some(d.path.clone()))),
            )
            .collect();
        let names: Vec<_> = locations.iter().map(|(name, _)| name.as_str()).collect();
        popover::filter_indices(flow.search.read(cx).text(), &names)
            .into_iter()
            .map(|ix| locations[ix].clone())
            .collect()
    }

    /// ListDrives on the flow's device (relay-forwarded when remote).
    /// Failures stay silent — the section just shows Home; the folder
    /// browser's own error row already covers "device didn't respond".
    fn load_space_drives(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        let flow_id = flow.id;
        flow.drives_request = flow.drives_request.wrapping_add(1);
        let request = flow.drives_request;
        flow.drives = Loadable::Loading;
        flow.drives_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            // Only target remote devices — local calls skip the relay.
            if let (Some(target), local) = (&device_id, &local)
                && local.as_deref() != Some(target.as_str())
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_DRIVES, serde_json::Value::Object(params))
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<DriveListing>(value).map_err(|error| error.to_string())
                });
            this.update(cx, |shell, cx| {
                shell.finish_space_drives_load(flow_id, request, device_id, result, cx);
            })
            .ok();
        }));
    }

    fn finish_space_drives_load(
        &mut self,
        flow_id: u64,
        request: u64,
        device_id: Option<String>,
        result: Result<DriveListing, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.id != flow_id
            || flow.drives_request != request
            || flow.device.as_ref().map(|d| d.id.as_str()) != device_id.as_deref()
        {
            return;
        }
        flow.drives = match result {
            Ok(listing) => Loadable::Ready(listing.drives),
            Err(error) => Loadable::Error(error),
        };
        cx.notify();
    }

    /// The current listing's folder rows filtered by the search query
    /// (prefix matches first — `popover::filter_indices`).
    fn add_space_filtered(&self, cx: &App) -> Vec<luna_proto::FolderEntry> {
        let Some(flow) = self.add_space.as_ref() else {
            return Vec::new();
        };
        if flow.step != ProjectStep::Folders {
            return Vec::new();
        }
        let Some(listing) = flow.browser.ready() else {
            return Vec::new();
        };
        let dirs = browser_rows(listing);
        let query = flow.search.read(cx).text().to_string();
        // A trailing slash on one folder name is still an ordinary folder
        // search; Return opens it. Slashes never cause a request while typing.
        let query = if let Some(folder) = query.strip_suffix('/')
            && !folder.contains('/')
            && !folder.starts_with('~')
        {
            folder
        } else {
            &query
        };
        let names: Vec<&str> = dirs.iter().map(|e| e.name.as_str()).collect();
        popover::filter_indices(query, &names)
            .into_iter()
            .map(|ix| dirs[ix].clone())
            .collect()
    }

    /// A path wins over the highlighted folder; otherwise open that folder.
    fn add_space_open_active(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if flow.step != ProjectStep::Devices {
            let query = flow.search.read(cx).text().to_string();
            if is_project_path_query(&query) {
                if let Some(target) =
                    crate::pickers::typed_path_target(&query, flow.home.as_deref())
                {
                    self.add_space_open_path(target, query.starts_with('~'), cx);
                } else if query.starts_with('~') {
                    // Resolve ~ on the selected device, even if the user
                    // pasted before its first Home listing completed.
                    self.add_space_goto_location("Home".into(), None, cx);
                    if let Some(flow) = self.add_space.as_mut() {
                        flow.pending_home_path = Some(query);
                    }
                }
                return;
            }
        }
        match flow.step {
            ProjectStep::Devices => {
                if let Some(device) = self.add_space_devices(cx).get(flow.active).cloned() {
                    self.add_space_pick_device(device, cx);
                }
                return;
            }
            ProjectStep::Locations => {
                if let Some((name, path)) = self.add_space_locations(cx).get(flow.active).cloned() {
                    self.add_space_goto_location(name, path, cx);
                }
                return;
            }
            ProjectStep::Folders => {}
        }
        let rows = self.add_space_filtered(cx);
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let Some(entry) = rows.get(flow.active) else {
            return;
        };
        let full = crate::pickers::child_path(&listing.path, &entry.name);
        let is_repo = entry.is_repo;
        let search = flow.search.clone();
        if let Some(flow) = self.add_space.as_mut() {
            flow.browser_repo = is_repo;
        }
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    fn add_space_open_path(&mut self, target: String, from_home: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.step = ProjectStep::Folders;
        flow.focus_pending = true;
        let search = flow.search.clone();
        search.update(cx, |input, cx| {
            input.set_placeholder("Search folders or paste a path…", cx);
        });
        let in_home = from_home
            || flow
                .home
                .as_deref()
                .is_some_and(|home| path_under(&target, home));
        // An arbitrary absolute path may lie outside Home or a displayed
        // volume. Root breadcrumbs remain honest in that case.
        flow.location = Some(if in_home {
            ("Home".into(), None)
        } else {
            ("/".into(), Some("/".into()))
        });
        flow.browser_repo = false;
        // Keep a typed destination editable until the device confirms it.
        self.load_space_folders(Some(target), cx);
    }

    /// The tab-completion target: the highlighted row when the query prefixes
    /// its name, else the first prefix match (filtering ranks those first).
    /// `(full name, remaining suffix)`; `None` on an empty query or when the
    /// match is already complete.
    fn add_space_completion(&self, cx: &App) -> Option<(String, String)> {
        let flow = self.add_space.as_ref()?;
        let query = flow.search.read(cx).text().to_string();
        if query.is_empty() {
            return None;
        }
        let rows = self.add_space_filtered(cx);
        let entry = rows
            .get(flow.active)
            .filter(|e| completion_prefix_len(&e.name, &query).is_some())
            .or_else(|| {
                rows.iter()
                    .find(|e| completion_prefix_len(&e.name, &query).is_some())
            })?;
        let len = completion_prefix_len(&entry.name, &query)?;
        if len >= entry.name.len() {
            return None;
        }
        Some((entry.name.clone(), entry.name[len..].to_string()))
    }

    /// ⇥: accept the completion — the query becomes the full folder name
    /// (the ghost the input was previewing). Descending stays on `/`/⏎.
    fn add_space_accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some((name, _)) = self.add_space_completion(cx) else {
            return;
        };
        if let Some(flow) = self.add_space.as_ref() {
            let search = flow.search.clone();
            search.update(cx, |input, cx| input.set_text(name, cx));
        }
    }

    /// Descend into a specific folder row (mouse path); clears the query.
    fn add_space_descend(&mut self, full: String, is_repo: bool, cx: &mut Context<Self>) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.browser_repo = is_repo;
        let search = flow.search.clone();
        search.update(cx, |input, cx| input.set_text("", cx));
        self.load_space_folders(Some(full), cx);
    }

    /// ListFolders on the flow's device (relay-forwarded when remote).
    pub(super) fn load_space_folders(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let local = self.state.read(cx).local_device_id.clone();
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.focus_pending = true;
        let device_id = flow.device.as_ref().map(|d| d.id.clone());
        let flow_id = flow.id;
        flow.folder_request = flow.folder_request.wrapping_add(1);
        let request = flow.folder_request;
        flow.browser_path = path.clone();
        flow.browser = Loadable::Loading;
        flow.active = 0;
        flow.list_scroll.set_offset(gpui::Point::default());
        let Some(engine) = engine else {
            flow.browser = Loadable::Error("Device is not connected".into());
            cx.notify();
            return;
        };
        flow.load_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::Map::new();
            if let Some(p) = &path {
                params.insert("path".into(), serde_json::Value::String(p.clone()));
            }
            // Only target remote devices — local calls skip the relay.
            if let (Some(target), local) = (&device_id, &local)
                && local.as_deref() != Some(target.as_str())
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target.clone()),
                );
            }
            let result = engine
                .client()
                .call(methods::LIST_FOLDERS, serde_json::Value::Object(params))
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<FolderListing>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |shell, cx| {
                shell.finish_space_folder_load(flow_id, request, device_id, path, result, cx);
            })
            .ok();
        }));
    }

    fn finish_space_folder_load(
        &mut self,
        flow_id: u64,
        request: u64,
        device_id: Option<String>,
        path: Option<String>,
        result: Result<FolderListing, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.id != flow_id
            || flow.folder_request != request
            || flow.step != ProjectStep::Folders
            || flow.device.as_ref().map(|d| d.id.as_str()) != device_id.as_deref()
            || flow.browser_path != path
        {
            return;
        }
        let went_home = path.is_none();
        let input_empty = flow.search.read(cx).is_empty();
        let pending_home_path = flow.pending_home_path.take();
        let jump_home_path = if went_home && result.is_ok() && input_empty {
            pending_home_path.clone()
        } else {
            None
        };
        let restore_home_query = if went_home && result.is_err() && input_empty {
            pending_home_path
        } else {
            None
        };
        let clear_confirmed_path = result.is_ok()
            && path.as_deref().is_some_and(|target| {
                let query = flow.search.read(cx).text();
                is_project_path_query(query)
                    && crate::pickers::typed_path_target(query, flow.home.as_deref()).as_deref()
                        == Some(target)
            });
        let search = flow.search.clone();
        flow.browser = match result {
            Ok(listing) => {
                if went_home {
                    flow.home = Some(listing.path.clone());
                }
                Loadable::Ready(listing)
            }
            Err(error) => Loadable::Error(error),
        };
        cx.notify();
        if clear_confirmed_path {
            search.update(cx, |input, cx| input.set_text("", cx));
        } else if let Some(query) = restore_home_query {
            search.update(cx, |input, cx| input.set_text(query, cx));
        }
        if let Some(query) = jump_home_path
            && let Some(home) = self
                .add_space
                .as_ref()
                .and_then(|flow| flow.home.as_deref())
            && let Some(target) = crate::pickers::typed_path_target(&query, Some(home))
        {
            self.add_space_open_path(target, true, cx);
        }
    }

    /// Create the space for the browser's current folder.
    fn submit_add_space(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(flow) = self.add_space.as_ref() else {
            return;
        };
        if flow.submit_busy || flow.step != ProjectStep::Folders {
            return;
        }
        let query = flow.search.read(cx).text();
        if is_project_path_query(query) {
            // The visible path is a destination request, not a filter on the
            // old listing. Return must resolve it before Add can use it.
            return;
        }
        let Some(device) = flow.device.clone() else {
            return;
        };
        let Some(listing) = flow.browser.ready() else {
            return;
        };
        let path = listing.path.clone();
        let git_detected = flow.browser_repo;
        let flow_id = flow.id;
        let origin = flow.origin.clone();
        // Same (device, folder) already has a space → just switch to it. The
        // engine dedupes this case too (a createSpace for a duplicate pair
        // no-ops), so creating would leave the minted id dangling.
        if let Some(existing) = existing_project_id(&self.state.read(cx).spaces, &device.id, &path)
        {
            self.finish_add_space_submission(flow_id, origin, existing, Ok(()), cx);
            return;
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        flow.submit_busy = true;
        flow.error = None;
        let space_id = uuid::Uuid::new_v4().to_string();
        // Optimistic echo: the watch frame carrying the real row replaces it
        // by id (apply_spaces re-sorts; same-id upsert is idempotent).
        let space = Space {
            execution_defaults: Default::default(),
            id: space_id.clone(),
            device_id: device.id.clone(),
            path: path.clone(),
            name: None,
            git_detected,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        };
        self.state.update(cx, |s, cx| {
            if !s.spaces.iter().any(|existing| existing.id == space.id) {
                s.spaces.push(space);
            }
            cx.notify();
        });
        let params = serde_json::json!({
            "op": "createSpace",
            "spaceId": space_id,
            "deviceId": device.id,
            "path": path,
            "gitDetected": git_detected,
        });
        let submit_id = space_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::MUTATE, params)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
            this.update(cx, |shell, cx| {
                shell.finish_add_space_submission(flow_id, origin, submit_id.clone(), result, cx);
            })
            .ok();
        });
        if let Some(flow) = self.add_space.as_mut() {
            flow.submit_task = Some(task);
        }
        cx.notify();
    }

    fn finish_add_space_submission(
        &mut self,
        flow_id: u64,
        origin: AddSpaceOrigin,
        space_id: String,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let is_current = self
            .add_space
            .as_ref()
            .is_some_and(|flow| flow.id == flow_id);
        match result {
            Ok(()) if is_current => {
                self.add_space = None;
                match origin {
                    AddSpaceOrigin::Standalone => self.land_in_space(space_id, cx),
                    AddSpaceOrigin::MoveTask { chat_id } => {
                        self.open_move_project_to(chat_id, space_id, cx)
                    }
                }
            }
            Ok(()) => {
                // A canceled or replaced flow may still finish upstream. The
                // catalog watch owns that new row; it must not navigate or
                // consume the origin of a newer flow.
            }
            Err(error) => {
                self.state.update(cx, |state, cx| {
                    state.spaces.retain(|space| space.id != space_id);
                    cx.notify();
                });
                if is_current && let Some(flow) = self.add_space.as_mut() {
                    flow.submit_busy = false;
                    flow.error = Some(error.into());
                }
            }
        }
        cx.notify();
    }

    /// Back traverses folders, then locations, then devices.
    fn add_space_go_up(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = &self.add_space else {
            return;
        };
        match flow.step {
            ProjectStep::Devices => return,
            ProjectStep::Locations => self.add_space_back_to(ProjectStep::Devices, cx),
            ProjectStep::Folders => {
                let listing = flow.browser.ready();
                let root = flow
                    .location
                    .as_ref()
                    .and_then(|(_, path)| path.as_deref())
                    .or(flow.home.as_deref());
                let parent = listing
                    .filter(|l| Some(l.path.as_str()) != root)
                    .and_then(|l| parent_path(&l.path));
                if let Some(parent) = parent {
                    self.add_space_descend(parent, false, cx);
                } else {
                    self.add_space_back_to(ProjectStep::Locations, cx);
                }
            }
        }
    }

    /// Palette keys (bubbling from the focused search input) — every legend
    /// maps to a REAL key: ↑↓ (or ctrl-n/p) navigate, →/⏎ open the
    /// highlighted folder, ← up a level, ⇥ completes the query to the
    /// previewed folder name, ⌘⏎ add the OPEN folder, ⌫ (empty query) also
    /// goes up, esc closes. Paths open only on Return, including pasted paths.
    fn add_space_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        // ←/→ act on the FOLDERS, not the text cursor — the palette is a
        // navigator first; queries are short and edited with ⌫.
        match event.keystroke.key.as_str() {
            "right" => {
                self.add_space_open_active(cx);
                return;
            }
            "left" => {
                self.add_space_go_up(cx);
                return;
            }
            // Unbound in "PaletteSearch" (like enter), so it bubbles here
            // instead of editing text or moving focus.
            "tab" => {
                self.add_space_accept_completion(cx);
                return;
            }
            _ => {}
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        match key {
            popover::MenuKey::Escape => {
                self.dismiss_add_space(cx);
                cx.stop_propagation();
            }
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = match self.add_space.as_ref().map(|f| f.step) {
                    Some(ProjectStep::Devices) => self.add_space_devices(cx).len(),
                    Some(ProjectStep::Locations) => self.add_space_locations(cx).len(),
                    _ => self.add_space_filtered(cx).len(),
                };
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(flow) = self.add_space.as_mut() {
                    flow.active = popover::menu_step(Some(flow.active), count, delta).unwrap_or(0);
                    // Keep the highlighted row in view as the cursor walks
                    // past the viewport (user-reported: the list didn't
                    // follow the keyboard).
                    flow.list_scroll.scroll_to_item(flow.active);
                    cx.notify();
                }
            }
            // ⏎ opens the highlighted folder (an alias for →); the space is
            // added with ⌘⏎ — and the chord acts on the folder OPEN in the
            // breadcrumbs, not the highlight. The highlight auto-rests on the
            // first row, so a chord that took it would add arbitrary
            // subfolders; the usual target (a repo root full of subfolders)
            // is only ever "the folder you're standing in".
            popover::MenuKey::Enter => self.add_space_open_active(cx),
            popover::MenuKey::ModEnter => self.submit_add_space(cx),
            popover::MenuKey::Backspace => {
                let empty = self
                    .add_space
                    .as_ref()
                    .is_some_and(|f| f.search.read(cx).is_empty());
                if empty {
                    self.add_space_go_up(cx);
                }
            }
            popover::MenuKey::Other => {}
        }
    }

    /// The same glass, header, row rhythm, scroll gutters and footer as Cmd+K.
    pub(super) fn render_add_space_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let flow = self.add_space.as_mut()?;
        if std::mem::take(&mut flow.focus_pending) {
            window.focus(&flow.search.focus_handle(cx), cx);
        }
        let step = flow.step;
        let search = flow.search.clone();
        let focus = flow.focus.clone();
        let scroll = flow.list_scroll.clone();
        let device = flow.device.clone();
        let listing = flow.browser.ready().cloned();
        let location = flow.location.clone();
        let home = flow.home.clone();
        let load_error = flow.browser.error().map(str::to_string);
        let error = flow.error.clone();
        let busy = flow.submit_busy;
        let active = flow.active;
        let loading = matches!(flow.browser, Loadable::Idle | Loadable::Loading);
        let drives_loading = matches!(flow.drives, Loadable::Loading);
        let ghost = self
            .add_space_completion(cx)
            .map(|(_, suffix)| SharedString::from(suffix));
        search.update(cx, |input, cx| {
            input.set_ghost(ghost, cx);
        });
        let query = search.read(cx).text().to_string();
        let path_query = step != ProjectStep::Devices && is_project_path_query(&query);
        let row = |ix: usize| {
            popover::menu_row(&theme, ix == active, format!("project-result-{ix}"))
                .id(("project-result", ix))
                .rounded(px(popover::PALETTE_ITEM_RADIUS))
                .h(px(32.0))
                .flex_none()
        };
        let mut rows = Vec::new();
        match step {
            ProjectStep::Devices => {
                for (ix, device) in self.add_space_devices(cx).into_iter().enumerate() {
                    let glyph = match device.platform.as_str() {
                        "macos" | "darwin" => icons::LAPTOP,
                        "web" => icons::GLOBAL,
                        "ios" | "android" => icons::SMARTPHONE,
                        _ => icons::MONITOR,
                    };
                    let online = self.state.read(cx).device_online(&device.id, Utc::now());
                    let name = device.name.clone();
                    rows.push(
                        row(ix)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_space_pick_device(device.clone(), cx)
                            }))
                            .child(icon(glyph).size(px(17.0)).text_color(theme.text_muted))
                            .child(popover::search_highlight(name.into(), Some(&query), &theme))
                            .child(div().flex_1())
                            .child(div().size(px(5.0)).rounded_full().bg(if online {
                                theme.success
                            } else {
                                theme.text_faint
                            }))
                            .into_any_element(),
                    );
                }
            }
            ProjectStep::Locations => {
                if path_query {
                    let target = crate::pickers::typed_path_target(&query, home.as_deref())
                        .unwrap_or_else(|| query.clone());
                    let device_name = device.as_ref().map(|d| d.name.as_str()).unwrap_or("device");
                    rows.push(
                        row(0)
                            .on_click(cx.listener(|this, _, _, cx| this.add_space_open_active(cx)))
                            .child(
                                icon(icons::FOLDER)
                                    .size(px(17.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("Open {target} on {device_name}")),
                            )
                            .into_any_element(),
                    );
                } else {
                    for (ix, (name, path)) in self.add_space_locations(cx).into_iter().enumerate() {
                        let glyph = if path.is_none() {
                            icons::HOME
                        } else {
                            icons::HARD_DRIVE
                        };
                        let label = name.clone();
                        rows.push(
                            row(ix)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_space_goto_location(name.clone(), path.clone(), cx)
                                }))
                                .child(icon(glyph).size(px(17.0)).text_color(theme.text_muted))
                                .child(popover::search_highlight(
                                    label.into(),
                                    Some(&query),
                                    &theme,
                                ))
                                .into_any_element(),
                        );
                    }
                }
            }
            ProjectStep::Folders => {
                if path_query {
                    {
                        let target = crate::pickers::typed_path_target(&query, home.as_deref())
                            .unwrap_or_else(|| query.clone());
                        let device_name =
                            device.as_ref().map(|d| d.name.as_str()).unwrap_or("device");
                        let label = format!("Open {target} on {device_name}");
                        rows.push(
                            row(0)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.add_space_open_active(cx)),
                                )
                                .child(
                                    icon(icons::FOLDER)
                                        .size(px(17.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(div().min_w_0().truncate().child(label))
                                .into_any_element(),
                        );
                    }
                } else if !loading && load_error.is_none() {
                    for (ix, entry) in self.add_space_filtered(cx).into_iter().enumerate() {
                        let base = listing.as_ref().map(|l| l.path.as_str()).unwrap_or("");
                        let full = crate::pickers::child_path(base, &entry.name);
                        let is_repo = entry.is_repo;
                        rows.push(
                            row(ix)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_space_descend(full.clone(), is_repo, cx)
                                }))
                                .child(
                                    icon(icons::FOLDER)
                                        .size(px(17.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(popover::search_highlight(
                                    entry.name.into(),
                                    Some(&query),
                                    &theme,
                                ))
                                .child(div().flex_1())
                                .when(is_repo, |el| {
                                    el.child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(14.0))
                                            .text_color(theme.text_muted),
                                    )
                                })
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        if let Some(flow) = self.add_space.as_mut() {
            flow.active = flow.active.min(rows.len().saturating_sub(1));
        }
        let empty = rows.is_empty();
        let mut results = div()
            .id("project-results")
            .max_h(px((f32::from(viewport.height) - 220.0).clamp(100.0, 424.0)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .px(px(popover::CARD_INSET))
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .children(rows);
        if step == ProjectStep::Folders && loading {
            results = results.child(popover::skeleton_rows(
                "project-loading",
                &theme,
                5,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(message) = load_error.filter(|_| step == ProjectStep::Folders) {
            results = results.child(
                popover::error_row(&theme, &message).p(px(14.0)).child(
                    popover::btn_ghost(&theme, "Retry", "project-retry")
                        .id("project-retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let typed_path = this.add_space.as_ref().is_some_and(|flow| {
                                is_project_path_query(flow.search.read(cx).text())
                            });
                            if typed_path {
                                this.add_space_open_active(cx);
                            } else {
                                let path =
                                    this.add_space.as_ref().and_then(|f| f.browser_path.clone());
                                this.load_space_folders(path, cx);
                            }
                        })),
                ),
            );
        } else if empty {
            results = results.child(div().p(px(24.0)).text_color(theme.text_muted).child(
                match step {
                    ProjectStep::Devices => "No devices found",
                    ProjectStep::Locations => "No locations found",
                    ProjectStep::Folders if path_query => "Open Home first to use ~ paths",
                    ProjectStep::Folders if query.is_empty() => "No folders here",
                    ProjectStep::Folders => "No folders match",
                },
            ));
        }
        if step == ProjectStep::Locations && drives_loading {
            results = results.child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_color(theme.text_muted)
                    .text_size(crate::typography::ui_rems(11.0))
                    .child("Loading locations…"),
            );
        }
        let crumb =
            |id: SharedString, name: SharedString, glyph: Option<&'static str>, current: bool| {
                div()
                    .id(id)
                    .h(px(26.0))
                    .px(px(7.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .text_color(if current {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .when(current, |el| el.bg(theme.element_hover))
                    .hover(|s| s.bg(theme.element_hover).text_color(theme.text))
                    .when_some(glyph, |el, glyph| {
                        el.child(
                            icon(glyph)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(if current {
                                    theme.text
                                } else {
                                    theme.text_muted
                                }),
                        )
                    })
                    .child(div().max_w(px(140.0)).truncate().child(name))
            };
        // Keep each chevron with its destination when a long path wraps.
        let segment = |item: gpui::Stateful<gpui::Div>| {
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(
                    icon(icons::ALT_ARROW_RIGHT)
                        .size(px(12.0))
                        .text_color(theme.text_faint),
                )
                .child(item)
        };
        let mut trail = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.0))
            .child(
                crumb(
                    "project-crumb-root".into(),
                    "New project".into(),
                    None,
                    step == ProjectStep::Devices,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.add_space_back_to(ProjectStep::Devices, cx)),
                ),
            );
        if let Some(device) = device {
            let glyph = match device.platform.as_str() {
                "macos" | "darwin" => icons::LAPTOP,
                "ios" | "android" => icons::SMARTPHONE,
                _ => icons::MONITOR,
            };
            trail =
                trail.child(segment(
                    crumb(
                        "project-crumb-device".into(),
                        device.name.into(),
                        Some(glyph),
                        step == ProjectStep::Locations,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_space_back_to(ProjectStep::Locations, cx)
                    })),
                ));
        }
        if let Some((name, path)) = location {
            let glyph = if path.is_none() {
                icons::HOME
            } else {
                icons::HARD_DRIVE
            };
            let root = path.clone().or(home);
            let at_root = listing
                .as_ref()
                .is_none_or(|l| root.as_deref() == Some(l.path.as_str()));
            trail = trail.child(segment(
                crumb(
                    "project-crumb-location".into(),
                    name.clone().into(),
                    Some(glyph),
                    at_root,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.add_space_goto_location(name.clone(), path.clone(), cx)
                })),
            ));
            if let Some(listing) = listing.as_ref() {
                for (ix, (name, full)) in breadcrumbs(&listing.path).into_iter().enumerate() {
                    if root.as_deref().is_some_and(|root| path_under(root, &full)) {
                        continue;
                    }
                    trail = trail.child(segment(
                        crumb(
                            format!("project-crumb-folder-{ix}").into(),
                            name.into(),
                            Some(icons::FOLDER),
                            full == listing.path,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_space_descend(full.clone(), false, cx)
                        })),
                    ));
                }
            }
        }
        let crumbs = div()
            .px(px(14.0))
            .py(px(8.0))
            .flex()
            .items_start()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .child(
                div()
                    .id("project-crumb-back")
                    .size(px(26.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.element_hover).text_color(theme.text))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_add_space(cx);
                        this.toggle_command_palette(window, cx);
                    }))
                    .child(
                        icon(icons::ARROW_LEFT)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mt(px(5.0))
                    .flex_none()
                    .bg(theme.border),
            )
            .child(trail);
        let header = div()
            .h(px(58.0))
            .flex_none()
            .px(px(18.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .border_b_1()
            .border_color(theme.border)
            .child(popover::palette_search_icon(&theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(crate::typography::ui_rems(15.0))
                    .child(search),
            )
            .child(popover::key_hint_text(&theme, "esc", ""));
        let footer = div()
            .flex_none()
            .px(px(18.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(18.0))
            .child(popover::key_hint_pair(
                &theme,
                icons::ARROW_UP,
                icons::ARROW_DOWN,
                "Navigate",
            ))
            .child(popover::key_hint_text(&theme, "↵", "Open"))
            .child(popover::key_hint_text(&theme, "esc", "Close"))
            .child(div().flex_1())
            .when(step == ProjectStep::Folders, |el| {
                el.child(
                    popover::btn_ghost(
                        &theme,
                        if busy { "Adding…" } else { "Add project" },
                        "project-add",
                    )
                    .id("project-add")
                    .h(px(22.0))
                    .py(px(0.0))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .when(busy || listing.is_none() || path_query, |el| {
                        el.opacity(0.5)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.submit_add_space(cx)))
                    .child(
                        popover::key_cap(&theme)
                            .text_size(crate::typography::ui_rems(11.0))
                            .child(crate::settings::badge_combo("mod-enter")),
                    ),
                )
            });
        let card =
            div()
                .id("add-space-palette")
                .track_focus(&focus)
                .w(px(600.0_f32.min(f32::from(viewport.width) - 32.0)))
                .flex()
                .flex_col()
                .rounded(px(14.0))
                .border_1()
                .border_color(theme.border)
                .when(!theme.is_frost(), |el| el.shadow_lg())
                .bg(popover::surface_bg(&theme))
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    this.add_space_key(event, cx)
                }))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.dismiss_add_space(cx);
                }))
                .child(header)
                .child(crumbs)
                .child(div().min_h_0().py(px(popover::CARD_INSET)).child(results))
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .px(px(18.0))
                            .pb(px(8.0))
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .child(footer);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .occlude()
                            .w(viewport.width)
                            .h(viewport.height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::frost::frosted(14.0, crate::frost::MENU_BLUR, card)),
                    ),
            )
            .priority(2)
            .into_any_element(),
        )
    }

    // ---- space context menu / rename / delete overlays ----

    fn close_space_menu(&mut self, cx: &mut Context<Self>) {
        if self.space_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.space_menu);
            cx.notify();
        }
    }

    pub(super) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.close_space_menu(cx);
        let current = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_default();
        let input = cx.new(|cx| ComposerInput::new("Project name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_space(cx);
            }
        });
        self.rename_space_dialog = Some(RenameSpaceDialog {
            space_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn submit_rename_space(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_space_dialog.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if !name.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameSpace", "spaceId": dialog.space_id, "name": name }),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.delete_space_confirm = None;
        let mut expected_chat_ids: Vec<String> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| chat.space_id.as_deref() == Some(&space_id))
            .map(|chat| chat.id.clone())
            .collect();
        expected_chat_ids.sort();
        self.mutate(
            serde_json::json!({ "op": "deleteSpace", "spaceId": space_id,
                "expectedChatIds": expected_chat_ids }),
            cx,
        );
        cx.notify();
    }

    /// Space context menu + rename dialog + delete confirm (appended to the
    /// shell's overlay list).
    pub(super) fn render_space_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some((space_id, position)) = self.space_menu.get().cloned() {
            let closing = self.space_menu.closing_since();
            let rename_id = space_id.clone();
            let delete_id = space_id.clone();
            let menu = popover::popover_card(&theme)
                .w(px(170.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_space_menu(cx);
                }))
                .flex()
                .flex_col()
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-rename-{space_id}"))
                        .id("space-menu-rename")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_rename_space(rename_id.clone(), cx)
                        }))
                        .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Rename…")),
                )
                .child(popover::menu_separator())
                .child(
                    popover::menu_row(&theme, false, format!("space-menu-delete-{space_id}"))
                        .id("space-menu-delete")
                        .text_color(theme.danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_space_menu(cx);
                            this.delete_space_confirm = Some(delete_id.clone());
                            cx.notify();
                        }))
                        .child(
                            icon(icons::TRASH_BIN_MINIMALISTIC)
                                .size(px(16.0))
                                .text_color(theme.danger),
                        )
                        .child(SharedString::from("Remove…")),
                )
                .into_any_element();
            overlays.push(popover::menu_at(
                "space-context-menu",
                position,
                menu,
                closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_space_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_space_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename project"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-space-cancel")
                                .id("rename-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_space_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-space-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_space(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-space-dialog", viewport, card));
        }

        if let Some(space_id) = self.delete_space_confirm.clone() {
            let (name, device, count) = {
                let state = self.state.read(cx);
                let space = state.space_row(&space_id);
                (
                    space
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "this project".into()),
                    space
                        .and_then(|s| state.device_name(&s.device_id))
                        .unwrap_or("its device")
                        .to_string(),
                    state
                        .chats
                        .iter()
                        .filter(|chat| chat.space_id.as_deref() == Some(&space_id))
                        .count(),
                )
            };
            let copy = if count == 1 {
                format!(
                    "Permanently delete “{name}” and its 1 LUNA session on {device}? This can’t be undone."
                )
            } else {
                format!(
                    "Permanently delete “{name}” and its {count} LUNA sessions on {device}? This can’t be undone."
                )
            };
            let card = popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Remove project?"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, copy)))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme,
                    "Every session in this project, including any fork listed in the count, is deleted. Forks outside this project, project files, shared attachments, synced copies and provider-private data remain. LUNA cleans up local transcript snapshots, unsent drafts, terminal history and verified unshared draft uploads. Remote cleanup is unavailable; deletion stops if this project contains remote sessions.",
                )))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "delete-space-cancel")
                                .id("delete-space-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.delete_space_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Remove")
                                .id("delete-space-confirm")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_space(space_id.clone(), cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal(
                "delete-space-dialog",
                viewport,
                self.confirmation_card(card, cx),
            ));
        }

        overlays
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use chrono::{TimeZone as _, Utc};

    use super::{
        activity_sidebar_bucket, activity_sidebar_chats, compare_sidebar_chats,
        promote_local_device_group,
    };
    use crate::settings::SidebarSort;
    use luna_proto::ChatIndicator;

    fn group(device: &str, value: u8) -> (Option<(String, String)>, Vec<u8>) {
        (Some((device.into(), device.into())), vec![value])
    }

    fn chat(id: &str) -> luna_proto::Chat {
        luna_proto::Chat {
            child_thread: None,
            id: id.into(),
            device_id: "device".into(),
            title: None,
            archived: false,
            settlement_override: None,
            pinned_order: None,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: Some(Utc.timestamp_opt(10, 0).unwrap()),
            created_at: Utc.timestamp_opt(5, 0).unwrap(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: None,
        }
    }

    #[test]
    fn activity_view_shows_actionable_and_running_tasks_not_idle_pins() {
        let mut idle = chat("idle-pinned");
        idle.pinned_order = Some("0".into());
        let approval = chat("approval");
        let input = chat("input");
        let error = chat("error");
        let unread = chat("unread");
        let queued_failure = chat("queued-failure");
        let running = chat("running");
        let attention =
            HashSet::from([(queued_failure.device_id.clone(), queued_failure.id.clone())]);
        let rows = activity_sidebar_chats(
            vec![
                (ChatIndicator::Idle, &idle),
                (ChatIndicator::Working, &running),
                (ChatIndicator::Completed, &unread),
                (ChatIndicator::Idle, &queued_failure),
                (ChatIndicator::Errored, &error),
                (ChatIndicator::AwaitingInput, &input),
                (ChatIndicator::AwaitingApproval, &approval),
            ],
            &attention,
        );
        assert_eq!(
            rows.into_iter()
                .map(|(_, chat)| chat.id.as_str())
                .collect::<Vec<_>>(),
            [
                "approval",
                "error",
                "input",
                "queued-failure",
                "unread",
                "running"
            ]
        );
        assert_eq!(activity_sidebar_bucket(ChatIndicator::Idle, false), None);
        assert_eq!(activity_sidebar_bucket(ChatIndicator::Idle, true), Some(0));
        assert_eq!(
            activity_sidebar_bucket(ChatIndicator::Working, false),
            Some(1)
        );
    }

    #[test]
    fn activity_attention_matches_the_exact_task_device() {
        let local = chat("same-id");
        let mut remote = local.clone();
        remote.device_id = "remote-device".into();
        let attention = HashSet::from([(local.device_id.clone(), local.id.clone())]);
        let rows = activity_sidebar_chats(
            vec![
                (ChatIndicator::Idle, &remote),
                (ChatIndicator::Idle, &local),
            ],
            &attention,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.device_id, "device");
    }

    #[test]
    fn equal_sidebar_timestamps_sort_by_stable_chat_id() {
        let alpha = chat("alpha");
        let beta = chat("beta");
        assert!(compare_sidebar_chats(SidebarSort::Created, &alpha, &beta).is_lt());
        assert!(compare_sidebar_chats(SidebarSort::LastUpdated, &alpha, &beta).is_lt());
    }

    #[test]
    fn global_pin_order_precedes_recency_and_filtered_subsets_keep_it() {
        let mut alpha = chat("alpha");
        alpha.pinned_order = Some("11".into());
        let mut beta = chat("beta");
        beta.pinned_order = Some("1".into());
        let recent = chat("recent");
        let mut rows = vec![alpha.clone(), recent, beta.clone()];
        rows.sort_by(|left, right| compare_sidebar_chats(SidebarSort::LastUpdated, left, right));
        assert_eq!(
            rows.iter().map(|chat| chat.id.as_str()).collect::<Vec<_>>(),
            ["beta", "alpha", "recent"]
        );
        // A project-filtered subset uses the same keys rather than maintaining
        // a second project-local order.
        assert!(compare_sidebar_chats(SidebarSort::Created, &beta, &alpha).is_lt());
    }

    #[test]
    fn current_device_is_promoted_without_resorting_remote_groups() {
        let mut groups = vec![
            group("recent-remote", 1),
            group("local", 2),
            group("older-remote", 3),
        ];

        promote_local_device_group(&mut groups, Some("local"));

        let order: Vec<_> = groups
            .iter()
            .map(|(group, _)| group.as_ref().unwrap().0.as_str())
            .collect();
        assert_eq!(order, ["local", "recent-remote", "older-remote"]);
    }

    #[test]
    fn missing_current_device_leaves_group_order_untouched() {
        let mut groups = vec![group("first", 1), group("second", 2)];
        let before = groups.clone();

        promote_local_device_group(&mut groups, Some("not-present"));

        assert_eq!(groups, before);
    }
}

/// Synthetic responses for the isolated native screenshot fixture only.
#[cfg(feature = "project-palette-fixture")]
impl Shell {
    pub fn fixture_project_responses(&mut self, cx: &mut Context<Self>) {
        if std::env::var_os("LUNA_FIXTURE_BACKGROUND").is_some() {
            self.composer
                .read(cx)
                .pickers()
                .clone()
                .update(cx, |pickers, cx| pickers.fixture_model_catalog(cx));
        }
        let Some(flow) = self.add_space.as_mut() else {
            return;
        };
        if flow.device.is_some() && !matches!(flow.drives, Loadable::Ready(_)) {
            flow.drives = Loadable::Ready(
                serde_json::from_value(serde_json::json!([
                    {"name":"Projects", "path":"/projects"},
                    {"name":"System", "path":"/"}
                ]))
                .unwrap(),
            );
            cx.notify();
        }
        if flow.step == ProjectStep::Folders && !matches!(flow.browser, Loadable::Ready(_)) {
            let path = flow
                .browser_path
                .clone()
                .unwrap_or_else(|| "/home/alex".into());
            if flow.browser_path.is_none() {
                flow.home = Some(path.clone());
            }
            let names = match path.as_str() {
                "/home/alex" => vec!["Desktop", "Documents", "Downloads", "Projects"],
                "/projects" | "/home/alex/Projects" => vec!["fieldnotes", "mobile-app", "website"],
                _ => vec!["assets", "docs", "src", "tests"],
            };
            flow.browser = Loadable::Ready(
                serde_json::from_value(serde_json::json!({
                    "path":path,
                    "entries":names.into_iter().map(|name| serde_json::json!({
                        "name":name,"isDir":true,"isRepo":name=="fieldnotes"
                    })).collect::<Vec<_>>()
                }))
                .unwrap(),
            );
            cx.notify();
        }
    }
}

#[cfg(test)]
mod project_flow_tests {
    use super::*;

    fn test_shell(data_dir: PathBuf, cx: &mut gpui::Context<Shell>) -> Shell {
        let state = cx.new(|_| AppState::new());
        Shell::new(
            state,
            EngineBootConfig {
                data_dir,
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: luna_proto::HarnessId::Mock,
            },
            cx,
        )
    }

    #[gpui::test]
    fn canceled_move_origin_cannot_leak_into_reopened_add_project(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data = tempfile::tempdir().unwrap();
        let data_dir = data.path().to_path_buf();
        let shell = cx.new(move |cx| test_shell(data_dir, cx));
        shell.update(cx, |shell, cx| {
            shell.open_add_space_for_move("task-a".into(), cx);
            let old = shell.add_space.as_ref().unwrap();
            let old_id = old.id;
            let old_origin = old.origin.clone();
            shell.dismiss_add_space(cx);
            shell.open_add_space(cx);
            let reopened_id = shell.add_space.as_ref().unwrap().id;

            shell.finish_add_space_submission(
                old_id,
                old_origin,
                "late-project".into(),
                Ok(()),
                cx,
            );

            let reopened = shell.add_space.as_ref().expect("reopened flow survives");
            assert_eq!(reopened.id, reopened_id);
            assert_eq!(reopened.origin, AddSpaceOrigin::Standalone);
            assert!(shell.move_project_dialog.is_none());
        });
    }

    #[gpui::test]
    fn replaced_add_project_ignores_old_completion_and_returns_current_move_origin(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data = tempfile::tempdir().unwrap();
        let data_dir = data.path().to_path_buf();
        let shell = cx.new(move |cx| test_shell(data_dir, cx));
        shell.update(cx, |shell, cx| {
            shell.open_add_space_for_move("task-a".into(), cx);
            let old = shell.add_space.as_ref().unwrap();
            let old_id = old.id;
            let old_origin = old.origin.clone();
            shell.open_add_space_for_move("task-b".into(), cx);
            let current = shell.add_space.as_ref().unwrap();
            let current_id = current.id;
            let current_origin = current.origin.clone();

            shell.finish_add_space_submission(old_id, old_origin, "old-project".into(), Ok(()), cx);
            assert_eq!(shell.add_space.as_ref().unwrap().id, current_id);
            assert!(shell.move_project_dialog.is_none());

            shell.finish_add_space_submission(
                current_id,
                current_origin,
                "new-project".into(),
                Ok(()),
                cx,
            );
            assert!(shell.add_space.is_none());
            let preview = shell
                .move_project_dialog
                .as_ref()
                .expect("current move preview reopens");
            assert_eq!(preview.chat_id, "task-b");
            assert_eq!(preview.selected_space_id.as_deref(), Some("new-project"));
        });
    }

    #[gpui::test]
    fn devices_locations_folders_and_back_clear_stale_state(cx: &mut gpui::TestAppContext) {
        let data = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let shell = cx.new(|cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.devices = serde_json::from_value(serde_json::json!([
                    {"id":"local","name":"Studio","platform":"macos","lastSeenAt":null},
                    {"id":"remote","name":"Server","platform":"linux","lastSeenAt":null}
                ]))
                .unwrap();
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data.path().into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.open_add_space(cx);
            assert_eq!(shell.add_space.as_ref().unwrap().step, ProjectStep::Devices);
            assert!(shell.add_space.as_ref().unwrap().device.is_none());
            let search = shell.add_space.as_ref().unwrap().search.clone();
            search.update(cx, |input, cx| input.set_text("server", cx));
            assert_eq!(shell.add_space_devices(cx).len(), 1);
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_mut().unwrap();
            assert_eq!(flow.step, ProjectStep::Locations);
            assert_eq!(flow.device.as_ref().unwrap().id, "remote");
            assert!(flow.search.read(cx).is_empty());
            search.update(cx, |input, cx| input.set_text("/srv/naïve project/", cx));
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.step, ProjectStep::Folders);
            assert_eq!(flow.device.as_ref().unwrap().id, "remote");
            assert_eq!(flow.browser_path.as_deref(), Some("/srv/naïve project"));
            let flow_id = flow.id;
            let request = flow.folder_request;
            shell.finish_space_folder_load(
                flow_id,
                request,
                Some("remote".into()),
                Some("/srv/naïve project".into()),
                Ok(FolderListing {
                    path: "/srv/naïve project".into(),
                    entries: Vec::new(),
                    truncated: false,
                }),
                cx,
            );
            assert!(search.read(cx).is_empty());
            assert_eq!(
                shell
                    .add_space
                    .as_ref()
                    .unwrap()
                    .browser
                    .ready()
                    .unwrap()
                    .path,
                "/srv/naïve project"
            );
            shell.add_space_back_to(ProjectStep::Locations, cx);
            let flow = shell.add_space.as_mut().unwrap();
            flow.drives = Loadable::Ready(vec![DriveEntry {
                name: "Projects".into(),
                path: "/projects".into(),
            }]);
            search.update(cx, |input, cx| input.set_text("projects", cx));
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_mut().unwrap();
            assert_eq!(flow.step, ProjectStep::Folders);
            assert_eq!(flow.browser_path.as_deref(), Some("/projects"));
            flow.browser = Loadable::Ready(FolderListing {
                path: "/projects".into(),
                entries: Vec::new(),
                truncated: false,
            });
            shell.add_space_go_up(cx);
            assert_eq!(
                shell.add_space.as_ref().unwrap().step,
                ProjectStep::Locations
            );
            assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
            shell.add_space_go_up(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.step, ProjectStep::Devices);
            assert!(flow.device.is_none());
            assert!(flow.drives.ready().is_none());
            assert!(flow.search.read(cx).is_empty());
            // A path in device search remains a device query.
            search.update(cx, |input, cx| input.set_text("/projects/", cx));
            shell.add_space_open_active(cx);
            assert_eq!(shell.add_space.as_ref().unwrap().step, ProjectStep::Devices);
        });
    }

    #[gpui::test]
    fn standalone_opens_local_home_and_paths_require_return(cx: &mut gpui::TestAppContext) {
        let data = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let shell = cx.new(|cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.local_device_id = Some("local".into());
                state.devices = serde_json::from_value(serde_json::json!([
                    {"id":"local","name":"Studio","platform":"macos","lastSeenAt":null},
                    {"id":"remote","name":"Server","platform":"linux","lastSeenAt":null}
                ]))
                .unwrap();
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data.path().into(),
                    ipc_port: 0,
                    edge_url: String::new(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.open_add_space(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.step, ProjectStep::Folders);
            assert_eq!(flow.device.as_ref().unwrap().id, "local");
            assert_eq!(flow.location.as_ref().unwrap().0, "Home");
            assert_eq!(flow.browser_path, None);
            let search = flow.search.clone();
            search.update(cx, |input, cx| {
                input.set_text("/Volumes/Work/naïve project/", cx)
            });
            assert_eq!(shell.add_space.as_ref().unwrap().browser_path, None);
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(
                flow.browser_path.as_deref(),
                Some("/Volumes/Work/naïve project")
            );
            assert_eq!(flow.location.as_ref().unwrap().0, "/");
            assert_eq!(flow.search.read(cx).text(), "/Volumes/Work/naïve project/");
            assert!(flow.browser.ready().is_none());

            // A tilde pasted before Home is known waits for that device's
            // Home listing, then opens the expanded path.
            search.update(cx, |input, cx| input.set_text("~/work space/", cx));
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.pending_home_path.as_deref(), Some("~/work space/"));
            let flow_id = flow.id;
            let request = flow.folder_request;
            shell.finish_space_folder_load(
                flow_id,
                request,
                Some("local".into()),
                None,
                Ok(FolderListing {
                    path: "/Users/éva".into(),
                    entries: Vec::new(),
                    truncated: false,
                }),
                cx,
            );
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.browser_path.as_deref(), Some("/Users/éva/work space"));
            assert_eq!(flow.location.as_ref().unwrap().0, "Home");
        });
    }

    #[gpui::test]
    fn local_home_lists_children_and_manual_open_keeps_add_target(cx: &mut gpui::TestAppContext) {
        let data = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let shell = cx.new(move |cx| test_shell(data.path().into(), cx));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.local_device_id = Some("local".into());
                state.devices = serde_json::from_value(serde_json::json!([
                    {"id":"local","name":"Studio","platform":"macos","lastSeenAt":null}
                ]))
                .unwrap();
                cx.notify();
            });
            shell.open_add_space(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.step, ProjectStep::Folders);
            assert_eq!(flow.browser_path, None);
            let flow_id = flow.id;
            let home_request = flow.folder_request;
            shell.finish_space_folder_load(
                flow_id,
                home_request,
                Some("local".into()),
                None,
                Ok(FolderListing {
                    path: "/Users/alex".into(),
                    entries: ["Desktop", "Documents", "Downloads"]
                        .into_iter()
                        .map(|name| luna_proto::FolderEntry {
                            name: name.into(),
                            is_dir: true,
                            is_repo: false,
                        })
                        .collect(),
                    truncated: false,
                }),
                cx,
            );
            assert_eq!(
                shell
                    .add_space_filtered(cx)
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>(),
                ["Desktop", "Documents", "Downloads"]
            );
            let search = shell.add_space.as_ref().unwrap().search.clone();
            search.update(cx, |input, cx| input.set_text("Doc", cx));
            assert_eq!(shell.add_space_filtered(cx).len(), 1);
            shell.add_space_open_active(cx);
            let flow = shell.add_space.as_ref().unwrap();
            assert_eq!(flow.browser_path.as_deref(), Some("/Users/alex/Documents"));
            assert!(flow.search.read(cx).is_empty());
            let child_request = flow.folder_request;
            shell.finish_space_folder_load(
                flow_id,
                child_request,
                Some("local".into()),
                Some("/Users/alex/Documents".into()),
                Ok(FolderListing {
                    path: "/Users/alex/Documents".into(),
                    entries: Vec::new(),
                    truncated: false,
                }),
                cx,
            );
            assert_eq!(
                shell
                    .add_space
                    .as_ref()
                    .unwrap()
                    .browser
                    .ready()
                    .unwrap()
                    .path,
                "/Users/alex/Documents"
            );
            shell.add_space_go_up(cx);
            assert_eq!(
                shell.add_space.as_ref().unwrap().browser_path.as_deref(),
                Some("/Users/alex")
            );
        });
    }

    #[gpui::test]
    fn stale_folder_and_drive_results_cannot_replace_current_device(cx: &mut gpui::TestAppContext) {
        let data = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let shell = cx.new(move |cx| test_shell(data.path().into(), cx));
        shell.update(cx, |shell, cx| {
            shell.open_add_space(cx);
            let first_id = shell.add_space.as_ref().unwrap().id;
            shell.add_space.as_mut().unwrap().step = ProjectStep::Folders;
            shell.add_space.as_mut().unwrap().device = Some(serde_json::from_value(
                serde_json::json!({"id":"local","name":"Studio","platform":"macos","lastSeenAt":null})
            ).unwrap());
            shell.add_space.as_mut().unwrap().browser_path = Some("/new".into());
            shell.add_space.as_mut().unwrap().folder_request = 2;
            shell.add_space.as_mut().unwrap().drives_request = 2;
            shell.add_space.as_mut().unwrap().pending_home_path = Some("~/old".into());
            let search = shell.add_space.as_ref().unwrap().search.clone();
            search.update(cx, |input, cx| input.set_text("/new", cx));
            shell.add_space.as_mut().unwrap().browser_path = None;
            shell.finish_space_folder_load(
                first_id, 2, Some("local".into()), None,
                Ok(FolderListing { path: "/home/local".into(), entries: Vec::new(), truncated: false }), cx,
            );
            assert_eq!(shell.add_space.as_ref().unwrap().browser_path, None);
            assert!(shell.add_space.as_ref().unwrap().pending_home_path.is_none());
            shell.add_space.as_mut().unwrap().browser_path = Some("/new".into());
            shell.add_space.as_mut().unwrap().browser = Loadable::Idle;
            let listing = |path: &str| FolderListing { path: path.into(), entries: Vec::new(), truncated: false };
            shell.finish_space_folder_load(first_id, 1, Some("local".into()), Some("/old".into()), Ok(listing("/old")), cx);
            assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
            shell.finish_space_folder_load(first_id, 2, Some("remote".into()), Some("/new".into()), Ok(listing("/new")), cx);
            assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
            shell.finish_space_drives_load(first_id, 1, Some("local".into()), Ok(DriveListing { drives: Vec::new() }), cx);
            assert!(shell.add_space.as_ref().unwrap().drives.ready().is_none());
            for error in ["No such file", "Not a directory", "permission denied"] {
                shell.finish_space_folder_load(first_id, 2, Some("local".into()), Some("/new".into()), Err(error.into()), cx);
                assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
                assert_eq!(shell.add_space.as_ref().unwrap().browser.error(), Some(error));
            }

            shell.open_add_space(cx);
            shell.finish_space_folder_load(first_id, 2, Some("local".into()), Some("/new".into()), Ok(listing("/new")), cx);
            assert!(shell.add_space.as_ref().unwrap().browser.ready().is_none());
        });
    }

    #[test]
    fn existing_project_match_requires_exact_device_and_path() {
        let project = Space {
            execution_defaults: Default::default(),
            id: "existing".into(),
            device_id: "local".into(),
            path: "/Users/éva/work space".into(),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        };
        assert_eq!(
            existing_project_id(&[project.clone()], "local", &project.path),
            Some("existing".into())
        );
        assert_eq!(
            existing_project_id(&[project.clone()], "remote", &project.path),
            None
        );
        assert_eq!(
            existing_project_id(&[project], "local", "/Users/éva/work space/"),
            None
        );
    }
}
