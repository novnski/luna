//! Tickets and chat board actions in the command palette (search), and the
//! board's recent activity in the Activity view. Every entry navigates by
//! stable ticket and chat ids; titles are only shown.
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use luna_proto::{BoardChatRole, BoardNavigationRequest, WorkItemEvent, WorkItemEventKind};

use super::dialogs::DialogKind;
use super::{ChatBoardPurpose, harness_label};
use crate::icons::{self, icon};
use crate::interaction::HoverFill as _;
use crate::popover;
use crate::shell::Shell;
use crate::theme::Theme;

/// Ticket matches the palette lists for one query.
const TICKET_RESULTS: usize = 8;
/// Board activity rows the Activity view shows.
const ACTIVITY_ROWS: usize = 6;

/// An action on the chat the palette was opened over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatAction {
    ConvertToTicket,
    CreateTicket,
    NewSideChat,
    HandOff,
}

impl ChatAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ConvertToTicket => "Convert this chat to a ticket…",
            Self::CreateTicket => "Create ticket from this chat…",
            Self::NewSideChat => "New side chat (fresh, same main chat)",
            Self::HandOff => "Hand off to a new main chat…",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::ConvertToTicket => icons::CHAT_TICKET,
            Self::CreateTicket => icons::CLIPBOARD_LIST,
            Self::NewSideChat => icons::PLUS,
            Self::HandOff => icons::ARROW_UP_RIGHT,
        }
    }
}

/// One board entry in the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoardPaletteEntry {
    /// An action on the current chat.
    Action(ChatAction),
    /// Open this ticket's detail.
    Ticket { work_item_id: String },
    /// Go to this ticket's working chat.
    GoToChat { work_item_id: String },
}

impl BoardPaletteEntry {
    /// Actions read like the palette's other commands.
    pub(crate) fn action(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Action(action) => Some((action.label(), action.icon())),
            _ => None,
        }
    }

    pub(crate) fn is_result(&self) -> bool {
        !matches!(self, Self::Action(_))
    }
}

fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

impl Shell {
    /// The chat the palette acts on: the workbench's focused tab while the
    /// selected chat's workbench shows, else the selected chat.
    fn board_palette_chat(&self, cx: &App) -> Option<String> {
        if !matches!(self.route, crate::shell::Route::Chat) {
            return None;
        }
        let selected = self.state.read(cx).selected_chat.clone()?;
        if self.workbench.main() == selected && self.workbench.loaded() {
            return self
                .workbench
                .layout()
                .focused_chat_id()
                .map(str::to_string)
                .or(Some(selected));
        }
        Some(selected)
    }

    /// The palette's board entries for `query` (lowercased): actions for
    /// the current chat, then (for a non-empty query) matching tickets.
    pub(in crate::shell) fn board_palette_entries(
        &self,
        query: &str,
        cx: &App,
    ) -> Vec<BoardPaletteEntry> {
        let mut entries = Vec::new();
        if let Some(chat_id) = self.board_palette_chat(cx) {
            let role = self.board_chat_role(&chat_id, cx);
            let archived = self
                .state
                .read(cx)
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .is_some_and(|chat| chat.archived);
            if !archived {
                if role.permitted.convert_to_ticket && role.purpose == ChatBoardPurpose::Ordinary {
                    entries.push(ChatAction::ConvertToTicket);
                }
                if role.purpose != ChatBoardPurpose::Reviewer {
                    entries.push(ChatAction::CreateTicket);
                }
                if role.role == BoardChatRole::Side {
                    if role.permitted.create_sibling_fresh {
                        entries.push(ChatAction::NewSideChat);
                    }
                    if role.permitted.create_main_with_handoff {
                        entries.push(ChatAction::HandOff);
                    }
                }
            }
        }
        let mut out: Vec<BoardPaletteEntry> = entries
            .into_iter()
            .filter(|action| matches(query, action.label()))
            .map(BoardPaletteEntry::Action)
            .collect();
        if query.trim().is_empty() {
            return out;
        }
        let Some(model) = self.board_chat.model() else {
            return out;
        };
        let model = model.read(cx);
        let mut shown = 0;
        for view in model.items() {
            if shown == TICKET_RESULTS {
                break;
            }
            let item = &view.item;
            let haystack = format!(
                "{} {} {} ticket",
                item.key,
                item.key.replace('-', ""),
                item.title
            );
            if !matches(query, &haystack) {
                continue;
            }
            shown += 1;
            out.push(BoardPaletteEntry::Ticket {
                work_item_id: item.id.clone(),
            });
            let has_chat =
                view.working_chat.is_some() || self.board_chat.working_chat_of(&item.id).is_some();
            if has_chat {
                out.push(BoardPaletteEntry::GoToChat {
                    work_item_id: item.id.clone(),
                });
            }
        }
        out
    }

    /// A ticket or Go to chat row (actions render as ordinary commands).
    pub(in crate::shell) fn render_board_palette_row(
        &self,
        entry: &BoardPaletteEntry,
        active: bool,
        index: usize,
        query: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (work_item_id, go) = match entry {
            BoardPaletteEntry::Ticket { work_item_id } => (work_item_id.clone(), false),
            BoardPaletteEntry::GoToChat { work_item_id } => (work_item_id.clone(), true),
            BoardPaletteEntry::Action(_) => return div().into_any_element(),
        };
        let view = self
            .board_chat
            .model()
            .and_then(|model| model.read(cx).item(&work_item_id).cloned());
        let (key, title, detail) = match &view {
            Some(view) => {
                let place = match &view.working_chat {
                    Some(chat) => match (chat.role, chat.main_parent_title.as_deref()) {
                        (BoardChatRole::Main, _) => "Main chat".to_string(),
                        (BoardChatRole::Side, Some(parent)) => format!("Side chat in {parent}"),
                        (BoardChatRole::Side, None) => "Side chat".to_string(),
                    },
                    None => "No working chat yet".to_string(),
                };
                let provider = view
                    .working_chat
                    .as_ref()
                    .and_then(|chat| chat.harness)
                    .map(|harness| format!(" · {}", harness_label(harness)))
                    .unwrap_or_default();
                (
                    view.item.key.clone(),
                    view.item.title.clone(),
                    format!(
                        "{} · {} · {place}{provider}",
                        view.item.column.label(),
                        view.phase.label()
                    ),
                )
            }
            None => ("Ticket".into(), "Ticket".into(), String::new()),
        };
        let label = if go {
            format!("Go to chat · {key} · {title}")
        } else {
            format!("{key} · {title}")
        };
        let probe = format!(
            "palette:{}:{work_item_id}",
            if go { "goto" } else { "ticket" }
        );
        self.board_mark(|| probe.clone(), || vec![label.clone(), detail.clone()]);
        let entry = entry.clone();
        popover::menu_row(theme, active, format!("board-palette-{index}"))
            .id(("board-palette", index))
            .rounded(px(popover::PALETTE_ITEM_RADIUS))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("{label}. {detail}")))
            .min_h(crate::typography::ui_rems(32.0))
            .py(crate::typography::ui_rems(4.0))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.close_command_palette(window, cx);
                this.activate_board_palette_entry(entry.clone(), cx);
            }))
            .child(
                icon(if go {
                    icons::ARROW_UP_RIGHT
                } else {
                    icons::CHAT_TICKET
                })
                .size(crate::typography::ui_rems(16.0))
                .flex_none()
                .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(popover::search_highlight(
                        SharedString::from(label.clone()),
                        Some(query),
                        theme,
                    ))
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_muted(0.6))
                            .child(SharedString::from(detail)),
                    ),
            )
            .relative()
            .children(self.board_probe_child(|| probe.clone()))
            .into_any_element()
    }

    /// Run a palette board entry (the palette has closed).
    pub(in crate::shell) fn activate_board_palette_entry(
        &mut self,
        entry: BoardPaletteEntry,
        cx: &mut Context<Self>,
    ) {
        match entry {
            BoardPaletteEntry::Ticket { work_item_id } => self.chat_open_ticket(work_item_id, cx),
            BoardPaletteEntry::GoToChat { work_item_id } => self.board_go_to_chat(
                BoardNavigationRequest {
                    work_item_id: Some(work_item_id),
                    task_id: None,
                },
                cx,
            ),
            BoardPaletteEntry::Action(action) => {
                let Some(chat_id) = self.board_palette_chat(cx) else {
                    return;
                };
                match action {
                    ChatAction::ConvertToTicket => {
                        self.board_open_dialog(DialogKind::Convert, chat_id, cx)
                    }
                    ChatAction::CreateTicket => {
                        self.board_open_dialog(DialogKind::CreateTicket, chat_id, cx)
                    }
                    ChatAction::NewSideChat => self.board_create_sibling(chat_id, cx),
                    ChatAction::HandOff => self.board_open_dialog(DialogKind::Handoff, chat_id, cx),
                }
            }
        }
    }

    /// The ticket key an event belongs to, from the Board list or badges.
    fn board_event_key(&self, event: &WorkItemEvent, cx: &App) -> String {
        if let WorkItemEventKind::Created { key, .. } = &event.kind {
            return key.clone();
        }
        self.board_chat
            .model()
            .and_then(|model| {
                model
                    .read(cx)
                    .item(&event.work_item_id)
                    .map(|view| view.item.key.clone())
            })
            .or_else(|| {
                self.board_chat
                    .badges
                    .values()
                    .chain(self.board_chat.related.values().flatten())
                    .find(|badge| badge.work_item_id == event.work_item_id)
                    .map(|badge| badge.key.clone())
            })
            .unwrap_or_else(|| "Ticket".into())
    }

    /// The Activity view's Board section: recent ticket activity, each row
    /// going to that ticket's chat (or the ticket when it has none).
    pub(in crate::shell) fn render_board_activity(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.activity.view_active || self.board_chat.activity().is_empty() {
            return None;
        }
        let events: Vec<WorkItemEvent> = self
            .board_chat
            .activity()
            .iter()
            .take(ACTIVITY_ROWS)
            .cloned()
            .collect();
        let now = chrono::Utc::now().timestamp_millis();
        let mut texts = Vec::new();
        let mut rows = Vec::new();
        for event in events {
            let key = self.board_event_key(&event, cx);
            let summary = event_summary(&event.kind);
            let ago = crate::state::format_time_ago(
                chrono::DateTime::from_timestamp_millis(event.at_ms).unwrap_or_default(),
                chrono::DateTime::from_timestamp_millis(now).unwrap_or_default(),
            );
            let label = format!("{key} · {summary}");
            texts.push(label.clone());
            let work_item_id = event.work_item_id.clone();
            let probe = format!("activity:{}", event.id);
            let id = format!("board-activity-{}", event.id);
            rows.push(
                div()
                    .id(SharedString::from(id.clone()))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("{label}, {ago}. Go to chat")))
                    .tab_index(0)
                    .relative()
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .hover_wash(id)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board_go_to_chat(
                            BoardNavigationRequest {
                                work_item_id: Some(work_item_id.clone()),
                                task_id: None,
                            },
                            cx,
                        )
                    }))
                    .on_key_down(crate::swallow_activation_key)
                    .child(
                        icon(icons::CHAT_TICKET)
                            .size(px(12.0))
                            .flex_none()
                            .text_color(theme.readable_muted(0.7)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text.opacity(0.85))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_muted(0.55))
                            .child(SharedString::from(ago)),
                    )
                    .children(self.board_probe_child(|| probe.clone()))
                    .into_any_element(),
            );
        }
        self.board_mark(|| "activity".into(), || texts);
        Some(
            div()
                .id("board-activity")
                .role(gpui::Role::Group)
                .aria_label("Board activity")
                .pt(px(10.0))
                .pr(px(8.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .px(px(8.0))
                        .pb(px(2.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.readable_muted(0.6))
                        .child("Board"),
                )
                .children(rows)
                .into_any_element(),
        )
    }
}

/// What happened, in the words Activity shows.
pub(crate) fn event_summary(kind: &WorkItemEventKind) -> String {
    match kind {
        WorkItemEventKind::Created { column, .. } => format!("Created in {}", column.label()),
        WorkItemEventKind::Updated { fields, .. } => format!("Edited {}", fields.join(", ")),
        WorkItemEventKind::Reordered { .. } => "Reordered".into(),
        WorkItemEventKind::Archived => "Archived".into(),
        WorkItemEventKind::Unarchived => "Unarchived".into(),
        WorkItemEventKind::Moved { to, .. } => format!("Moved to {}", to.label()),
        WorkItemEventKind::Completed { .. } => "Done".into(),
        WorkItemEventKind::Reopened { .. } => "Reopened".into(),
        WorkItemEventKind::LaunchAccepted { .. } => "Started work".into(),
        WorkItemEventKind::LaunchQueued { .. } => "Queued".into(),
        WorkItemEventKind::LaunchUnknown { .. } => "Start outcome unknown".into(),
        WorkItemEventKind::StopRequested { .. } => "Stop requested".into(),
        WorkItemEventKind::StopSettled { stopped, .. } => {
            if *stopped {
                "Stopped".into()
            } else {
                "Stop not confirmed".into()
            }
        }
        WorkItemEventKind::StatusReported { .. } => "Status reported".into(),
        WorkItemEventKind::BriefSent { .. } => "Brief sent".into(),
        WorkItemEventKind::ResultDelivered { .. } => "Result delivered".into(),
        WorkItemEventKind::AttentionRaised { .. } => "Needs attention".into(),
        WorkItemEventKind::ChatLinked { role, .. } => format!("Chat linked ({})", role.as_str()),
        WorkItemEventKind::ChatUnlinked { role, .. } => {
            format!("Chat unlinked ({})", role.as_str())
        }
        WorkItemEventKind::Converted { .. } => "Converted from a chat".into(),
        WorkItemEventKind::Renamed { title, .. } => format!("Renamed to “{title}”"),
        WorkItemEventKind::ReviewStarted { .. } => "Review started".into(),
        WorkItemEventKind::ReviewStateChanged { state, .. } => {
            format!("Review {}", state.as_str())
        }
        WorkItemEventKind::ReviewResult {
            blocking_findings, ..
        } => {
            if *blocking_findings == 0 {
                "Review passed".into()
            } else {
                format!(
                    "Review: {blocking_findings} blocking finding{}",
                    if *blocking_findings == 1 { "" } else { "s" }
                )
            }
        }
        WorkItemEventKind::ReviewerCleaned { .. } => "Reviewer closed".into(),
        WorkItemEventKind::OutcomeRecorded { outcome, .. } => {
            format!("Outcome recorded ({})", outcome.as_str())
        }
        WorkItemEventKind::IssueLinked { .. } => "Issue linked".into(),
        WorkItemEventKind::IssueUnlinked { .. } => "Issue unlinked".into(),
        WorkItemEventKind::IssueSourceChanged { .. } => "Issue changed".into(),
        WorkItemEventKind::DependencyAdded { .. } => "Blocker added".into(),
        WorkItemEventKind::DependencyRemoved { .. } => "Blocker removed".into(),
        WorkItemEventKind::IssueWriteback { .. } => "Written back to the issue".into(),
        WorkItemEventKind::Unknown => "Updated".into(),
    }
}
