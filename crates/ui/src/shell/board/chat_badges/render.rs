//! Badges, the working chat's header strip and menu rows. Related tickets
//! are rows of the right sidebar's Related list (`shell/related_chats.rs`).
//!
//! Every badge carries its meaning in a glyph and words (`Ticket LUN-42`,
//! `Reviewer · LUN-42`), with an accessible label and a tooltip naming the
//! ticket, its workflow state and the provider; colour only reinforces it.
use gpui::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px,
};
use luna_proto::{BoardChatRole, BoardColumn, ChatTicketBadge, TaskLinkRole};

use super::dialogs::DialogKind;
use super::{ChatBoardPurpose, StatusMenu, harness_label};
use crate::icons::{self, icon};
use crate::interaction::HoverFill as _;
use crate::popover;
use crate::shell::Shell;
use crate::theme::Theme;

/// `Ticket LUN-42` or `Reviewer · LUN-42`.
pub(crate) fn badge_text(badge: &ChatTicketBadge) -> String {
    match badge.role {
        TaskLinkRole::Reviewer => format!("Reviewer · {}", badge.key),
        _ => format!("Ticket {}", badge.key),
    }
}

/// The compact text on rows and tabs: the key, or `Reviewer · KEY`.
pub(crate) fn compact_text(badge: &ChatTicketBadge) -> String {
    match badge.role {
        TaskLinkRole::Reviewer => format!("Reviewer · {}", badge.key),
        _ => badge.key.clone(),
    }
}

fn badge_glyph(badge: &ChatTicketBadge) -> &'static str {
    match badge.role {
        TaskLinkRole::Reviewer => icons::CHAT_REVIEWER,
        _ => icons::CHAT_TICKET,
    }
}

/// What a screen reader hears for a badge.
pub(crate) fn badge_accessible(badge: &ChatTicketBadge) -> String {
    match badge.role {
        TaskLinkRole::Reviewer => format!(
            "Reviewer for ticket {}: {}. {} · {}",
            badge.key,
            badge.title,
            badge.column.label(),
            badge.phase.label()
        ),
        _ => format!(
            "Working chat of ticket {}: {}. {} · {}",
            badge.key,
            badge.title,
            badge.column.label(),
            badge.phase.label()
        ),
    }
}

/// The badge tooltip: the ticket, then where it stands and who runs it.
fn badge_tooltip(badge: &ChatTicketBadge) -> (SharedString, SharedString) {
    let title: SharedString = format!("{} · {}", badge_text(badge), badge.title).into();
    let mut detail = format!("{} · {}", badge.column.label(), badge.phase.label());
    if let Some(harness) = badge.harness {
        detail.push_str(" · ");
        detail.push_str(harness_label(harness));
    }
    detail.push_str(match badge.role {
        TaskLinkRole::Reviewer => {
            ". A temporary reviewer: it reports to the ticket's working chat."
        }
        _ => ". This chat is the ticket's working chat.",
    });
    (title, detail.into())
}

/// The workflow-status words: column, then the engine-observed phase, and
/// "Archived" for a working chat whose ticket was archived.
pub(crate) fn status_text(badge: &ChatTicketBadge) -> String {
    let mut text = format!("{} · {}", badge.column.label(), badge.phase.label());
    if badge.archived {
        text.push_str(" · Archived");
    }
    text
}

fn column_glyph(column: BoardColumn) -> &'static str {
    match column {
        BoardColumn::Backlog => icons::STOP,
        BoardColumn::Ready => icons::PLAY,
        BoardColumn::Done => icons::CHECK,
    }
}

/// Plate colours: the working badge on the accent wash, the reviewer on a
/// neutral plate (the glyph and words differ as well).
fn plate(badge: &ChatTicketBadge, theme: &Theme) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match badge.role {
        TaskLinkRole::Reviewer => {
            let bg = theme.fill(crate::theme::FillTier::Secondary);
            (
                bg,
                theme.readable_on(theme.text_muted, bg, crate::theme::TEXT_CONTRAST_TARGET),
                theme.readable_on(theme.text_muted, bg, crate::theme::NON_TEXT_CONTRAST_TARGET),
            )
        }
        _ => {
            let bg = theme.accent_wash;
            (
                bg,
                theme.readable_on_tab_badge(theme.accent, bg, crate::theme::TEXT_CONTRAST_TARGET),
                theme.readable_on_tab_badge(
                    theme.accent,
                    bg,
                    crate::theme::NON_TEXT_CONTRAST_TARGET,
                ),
            )
        }
    }
}

/// The compact badge for sidebar rows, side-chat tabs and side-chat lists.
pub(crate) fn compact_badge(
    badge: &ChatTicketBadge,
    id: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let (bg, fg, glyph) = plate(badge, theme);
    let (tooltip_title, tooltip_detail) = badge_tooltip(badge);
    div()
        .id(id.into())
        .role(gpui::Role::Label)
        .aria_label(SharedString::from(badge_accessible(badge)))
        .h(px(16.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.0))
        .px(px(4.0))
        .rounded(px(4.0))
        .bg(bg)
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(fg)
        .tooltip(move |_, cx| {
            crate::pickers::ControlTooltip::view(
                tooltip_title.clone(),
                Some(tooltip_detail.clone()),
                cx,
            )
        })
        .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
        .child(
            icon(badge_glyph(badge))
                .size(px(10.0))
                .flex_none()
                .text_color(glyph),
        )
        .child(SharedString::from(compact_text(badge)))
}

/// A small text button in the chat strip and receipts.
fn strip_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    glyph: Option<&'static str>,
    theme: &Theme,
) -> Stateful<Div> {
    let id = id.into();
    let label = label.into();
    div()
        .id(id.clone())
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .tab_index(0)
        .relative()
        .h(px(22.0))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .px(px(7.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.hairline(0.10))
        .text_size(crate::typography::ui_rems(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text.opacity(0.9))
        .cursor_pointer()
        .hover_wash(id)
        .focus_visible({
            let ring = theme.focus_ring();
            move |style| crate::theme::focus_ring(style, ring)
        })
        .on_key_down(crate::swallow_activation_key)
        .when_some(glyph, |el, glyph| {
            el.child(
                icon(glyph)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.readable_muted(0.8)),
            )
        })
        .child(label)
}

impl Shell {
    /// The compact badge on a chat row (main sidebar, Related chats list,
    /// palette), keyed by the row's own id. `None` for an ordinary chat.
    pub(in crate::shell) fn board_row_badge(
        &self,
        chat_id: &str,
        row_id: &str,
        theme: &Theme,
    ) -> Option<AnyElement> {
        let badge = self.board_chat.badge(chat_id)?;
        let label = format!("row-badge:{row_id}");
        self.board_mark(
            || label.clone(),
            || vec![compact_text(badge), badge_accessible(badge)],
        );
        Some(
            compact_badge(badge, format!("{row_id}-ticket"), theme)
                .relative()
                .children(self.board_probe_child(|| label.clone()))
                .into_any_element(),
        )
    }

    /// The compact badge on a side-chat (or any) workbench tab.
    pub(in crate::shell) fn board_tab_badge(
        &self,
        chat_id: &str,
        theme: &Theme,
    ) -> Option<AnyElement> {
        let badge = self.board_chat.badge(chat_id)?;
        let label = format!("tab-badge:{chat_id}");
        self.board_mark(
            || label.clone(),
            || vec![compact_text(badge), badge_accessible(badge)],
        );
        Some(
            compact_badge(badge, format!("wb-tab-ticket-{chat_id}"), theme)
                .relative()
                .children(self.board_probe_child(|| label.clone()))
                .into_any_element(),
        )
    }

    /// What a tab's accessible label adds for its ticket.
    pub(in crate::shell) fn board_tab_accessible(&self, chat_id: &str) -> Option<String> {
        self.board_chat
            .badge(chat_id)
            .map(|badge| match badge.role {
                TaskLinkRole::Reviewer => format!(", reviewer for ticket {}", badge.key),
                _ => format!(", ticket {}", badge.key),
            })
    }

    /// The chat's board strip, under its tab strip: the working chat's
    /// `Ticket KEY` badge and workflow-status control, or a reviewer's
    /// `Reviewer · KEY`. `None` (nothing drawn) for any other chat,
    /// including one with related tickets (the right sidebar lists them).
    pub(in crate::shell) fn render_board_chat_strip(
        &mut self,
        chat_id: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // A pending Go to chat finishes once the workbench has drawn its
        // layout: after this frame, never in the middle of it.
        if (self.board_chat.reveal.is_some() || self.board_chat.awaiting.is_some())
            && !self.board_chat.reveal_deferred
        {
            self.board_chat.reveal_deferred = true;
            let shell = cx.entity().downgrade();
            cx.defer(move |cx| {
                shell
                    .update(cx, |shell, cx| {
                        shell.board_chat.reveal_deferred = false;
                        shell.board_chat_settle_reveal(cx);
                    })
                    .ok();
            });
        }
        // Only the chat's own identity paints here. Its related tickets are
        // rows of the right sidebar's Related list, never part of the
        // conversation (a source chat with five tickets stays unobstructed).
        let chat_id = chat_id?.to_string();
        let badge = self.board_chat.badge(&chat_id).cloned()?;
        let mut texts = Vec::new();
        let row = self.render_board_header_row(&chat_id, &badge, theme, &mut texts, cx);
        let label = format!("strip:{chat_id}");
        self.board_mark(|| label.clone(), || texts);
        Some(
            div()
                .id(SharedString::from(format!("board-chat-strip-{chat_id}")))
                .role(gpui::Role::Group)
                .aria_label("Board")
                .w_full()
                .flex_none()
                .flex()
                .flex_col()
                .py(px(6.0))
                .border_b_1()
                .border_color(theme.hairline(0.06))
                .child(row)
                .relative()
                .children(self.board_probe_child(|| label.clone()))
                .into_any_element(),
        )
    }

    /// `[Ticket LUN-42] [Ready · Working ▾] … Main chat · Codex`.
    fn render_board_header_row(
        &mut self,
        chat_id: &str,
        badge: &ChatTicketBadge,
        theme: &Theme,
        texts: &mut Vec<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let role = self.board_chat_role(chat_id, cx);
        let (bg, fg, glyph) = plate(badge, theme);
        let (tooltip_title, tooltip_detail) = badge_tooltip(badge);
        let work_item_id = badge.work_item_id.clone();
        let reviewer = badge.role == TaskLinkRole::Reviewer;
        let badge_label = if reviewer {
            format!("header-reviewer:{chat_id}")
        } else {
            format!("header-ticket:{chat_id}")
        };
        texts.push(badge_text(badge));
        let ticket = div()
            .id(SharedString::from(format!("board-header-ticket-{chat_id}")))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!(
                "{}. Open ticket",
                badge_accessible(badge)
            )))
            .tab_index(0)
            .h(px(22.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .px(px(7.0))
            .rounded(px(6.0))
            .bg(bg)
            .text_size(crate::typography::ui_rems(12.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(fg)
            .cursor_pointer()
            .focus_visible({
                let ring = theme.focus_ring();
                move |style| crate::theme::focus_ring(style, ring)
            })
            .tooltip(move |_, cx| {
                crate::pickers::ControlTooltip::view(
                    tooltip_title.clone(),
                    Some(tooltip_detail.clone()),
                    cx,
                )
            })
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.chat_open_ticket(work_item_id.clone(), cx);
            }))
            .on_key_down(crate::swallow_activation_key)
            .child(
                icon(badge_glyph(badge))
                    .size(px(13.0))
                    .flex_none()
                    .text_color(glyph),
            )
            .child(SharedString::from(badge_text(badge)))
            .relative()
            .children(self.board_probe_child(|| badge_label.clone()));
        // The workflow-status control: a separate button, never the badge.
        let status = (!reviewer).then(|| {
            let status = status_text(badge);
            texts.push(status.clone());
            let status_label = format!("header-status:{chat_id}");
            let menu_task = chat_id.to_string();
            let menu_item = badge.work_item_id.clone();
            strip_button(
                format!("board-header-status-{chat_id}"),
                status.clone(),
                Some(column_glyph(badge.column)),
                theme,
            )
            .aria_label(SharedString::from(format!(
                "Workflow status: {status}. Change status"
            )))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                this.board_chat.status_menu = Some(StatusMenu {
                    task_id: menu_task.clone(),
                    work_item_id: menu_item.clone(),
                    position: event.position(),
                });
                cx.notify();
            }))
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(10.0))
                    .flex_none()
                    .text_color(theme.readable_muted(0.7)),
            )
            .children(self.board_probe_child(|| status_label.clone()))
        });
        let reviewer_phase = reviewer.then(|| {
            let text = format!("Reviewing {} · {}", badge.key, badge.phase.label());
            texts.push(text.clone());
            div()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.readable_muted(0.75))
                .child(SharedString::from(text))
        });
        // Where it is and who runs it: context only, never a control.
        let mut location = match (role.role, role.main_parent_id.as_deref()) {
            (BoardChatRole::Main, _) => "Main chat".to_string(),
            (BoardChatRole::Side, Some(main)) => {
                let title = self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|chat| chat.id == main)
                    .and_then(|chat| chat.title.as_deref())
                    .map(crate::transcript::single_line)
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| "an unavailable chat".into());
                format!("Side chat in {title}")
            }
            (BoardChatRole::Side, None) => "Side chat".to_string(),
        };
        if let Some(harness) = badge.harness {
            location.push_str(" · ");
            location.push_str(harness_label(harness));
        }
        texts.push(location.clone());
        div()
            .px(crate::typography::ui_rems(12.0))
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(ticket)
            .children(status)
            .children(reviewer_phase)
            .child(div().flex_1().min_w_0())
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.readable_muted(0.6))
                    .child(SharedString::from(location)),
            )
            .into_any_element()
    }

    /// Board rows for a chat's menus (the sidebar chat menu and the
    /// workbench tab menu): Open ticket, Convert, Create ticket, and a side
    /// chat's New side chat and Hand off. `close` closes the host menu. The
    /// host asks for the chat's identity when the menu opens
    /// ([`Shell::board_chat_ensure_identity`]); until it loads, the
    /// published lineage decides which rows show.
    pub(in crate::shell) fn board_chat_menu_rows(
        &self,
        chat_id: &str,
        archived: bool,
        close: fn(&mut Shell, &mut Context<Shell>),
        menu_key: &str,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let role = self.board_chat_role(chat_id, cx);
        let badge = self.board_chat.badge(chat_id).cloned();
        let mut rows: Vec<MenuRow> = Vec::new();
        if let Some(badge) = badge.as_ref() {
            rows.push((
                "open-ticket",
                badge_glyph(badge),
                format!("Open ticket {}", badge.key),
                None,
                Some(RowAction::OpenTicket(badge.work_item_id.clone())),
            ));
        }
        if !archived
            && role.permitted.convert_to_ticket
            && role.purpose == ChatBoardPurpose::Ordinary
        {
            rows.push((
                "convert",
                icons::CHAT_TICKET,
                "Convert this chat to a ticket…".into(),
                None,
                Some(RowAction::Dialog(DialogKind::Convert)),
            ));
        }
        if !archived && role.purpose != ChatBoardPurpose::Reviewer {
            rows.push((
                "create-ticket",
                icons::CLIPBOARD_LIST,
                "Create ticket from this chat…".into(),
                None,
                Some(RowAction::Dialog(DialogKind::CreateTicket)),
            ));
        }
        if role.role == BoardChatRole::Side && !archived {
            if role.permitted.create_sibling_fresh {
                rows.push((
                    "new-side",
                    icons::PLUS,
                    "New side chat".into(),
                    None,
                    Some(RowAction::Sibling),
                ));
            }
            if role.permitted.create_main_with_handoff {
                rows.push((
                    "handoff",
                    icons::ARROW_UP_RIGHT,
                    "Hand off to a new main chat…".into(),
                    None,
                    Some(RowAction::Dialog(DialogKind::Handoff)),
                ));
            }
            // Said, not silently missing: why there is no context fork here.
            rows.push((
                "no-fork",
                icons::INFO_CIRCLE,
                "No context forks from a side chat".into(),
                Some(super::SIDE_FORK_REFUSAL.into()),
                None,
            ));
        }
        let marks: Vec<String> = rows.iter().map(|row| row.2.clone()).collect();
        let mark_key = format!("menu:{menu_key}:{chat_id}");
        self.board_mark(|| mark_key.clone(), || marks);
        let mut out = Vec::new();
        if !rows.is_empty() {
            out.push(popover::menu_separator().into_any_element());
        }
        for (id, glyph, text, explanation, action) in rows {
            let row_id = format!("board-menu-{menu_key}-{id}-{chat_id}");
            let probe = format!("menu:{menu_key}:{id}:{chat_id}");
            let chat = chat_id.to_string();
            let mut row = popover::menu_row(theme, false, row_id.clone())
                .id(SharedString::from(row_id))
                .role(gpui::Role::MenuItem)
                .aria_label(SharedString::from(match &explanation {
                    Some(why) => format!("{text}. {why}"),
                    None => text.clone(),
                }))
                .child(icon(glyph).size(px(16.0)).text_color(theme.text_muted))
                .child(div().flex_1().min_w_0().child(SharedString::from(text)));
            if let Some(why) = explanation {
                let why: SharedString = why.into();
                row = row
                    .cursor_default()
                    .text_color(theme.readable_muted(0.6))
                    .tooltip(move |_, cx| {
                        crate::pickers::ControlTooltip::view(
                            "Side chats stay one level deep",
                            Some(why.clone()),
                            cx,
                        )
                    })
                    .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY);
            }
            if let Some(action) = action {
                row = row.on_click(cx.listener(move |this, _, _, cx| {
                    close(this, cx);
                    match &action {
                        RowAction::OpenTicket(work_item_id) => {
                            this.chat_open_ticket(work_item_id.clone(), cx)
                        }
                        RowAction::Dialog(kind) => this.board_open_dialog(*kind, chat.clone(), cx),
                        RowAction::Sibling => this.board_create_sibling(chat.clone(), cx),
                    }
                }));
            }
            out.push(
                row.relative()
                    .children(self.board_probe_child(|| probe.clone()))
                    .into_any_element(),
            );
        }
        out
    }

    /// The working chat's status menu: Start/Continue, Stop or Reopen (the
    /// engine's permitted moves when the Board list knows them). Done is
    /// engine-owned and never offered.
    pub(in crate::shell) fn render_board_status_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.board_chat.status_menu.clone()?;
        let Some(badge) = self.board_chat.badge(&menu.task_id).cloned() else {
            self.board_chat.status_menu = None;
            return None;
        };
        let theme = Theme::of(cx).for_popup();
        let actions = self
            .board_chat
            .model()
            .and_then(|model| {
                model
                    .read(cx)
                    .item(&menu.work_item_id)
                    .map(|view| view.actions.clone())
            })
            .unwrap_or_default();
        let allowed =
            |action: luna_proto::BoardAction| actions.is_empty() || actions.contains(&action);
        let mut rows: Vec<(&'static str, &'static str, String, BoardColumn)> = Vec::new();
        match badge.column {
            BoardColumn::Backlog => {
                let continuing = badge.phase != luna_proto::BoardPhase::NotStarted;
                let action = if continuing {
                    luna_proto::BoardAction::Continue
                } else {
                    luna_proto::BoardAction::Start
                };
                if allowed(action) || allowed(luna_proto::BoardAction::MoveToReady) {
                    rows.push((
                        "ready",
                        icons::PLAY,
                        if continuing {
                            "Continue · move to Ready".into()
                        } else {
                            "Start · move to Ready".into()
                        },
                        BoardColumn::Ready,
                    ));
                }
            }
            BoardColumn::Ready => {
                if allowed(luna_proto::BoardAction::Stop)
                    || allowed(luna_proto::BoardAction::MoveToBacklog)
                {
                    rows.push((
                        "backlog",
                        icons::STOP,
                        "Stop · move to Backlog".into(),
                        BoardColumn::Backlog,
                    ));
                }
            }
            BoardColumn::Done => {
                if allowed(luna_proto::BoardAction::Reopen)
                    || allowed(luna_proto::BoardAction::MoveToBacklog)
                {
                    rows.push((
                        "reopen",
                        icons::RESTART,
                        "Reopen · move to Backlog".into(),
                        BoardColumn::Backlog,
                    ));
                }
            }
        }
        let mut marks: Vec<String> = rows.iter().map(|row| row.2.clone()).collect();
        marks.push(format!("Open ticket {}", badge.key));
        self.board_mark(|| "status-menu".into(), || marks);
        let mut card = popover::popover_card(&theme)
            .w(crate::typography::ui_rems(248.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.board_chat.status_menu = None;
                cx.notify();
            }))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(crate::typography::ui_rems(8.0))
                    .py(crate::typography::ui_rems(4.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.readable_muted(0.7))
                    .child(SharedString::from(format!(
                        "{} · {}",
                        badge.key,
                        status_text(&badge)
                    ))),
            );
        for (id, glyph, text, to) in rows {
            let work_item_id = menu.work_item_id.clone();
            let probe = format!("status-menu:{id}");
            card = card.child(
                popover::menu_row(&theme, false, format!("board-status-{id}"))
                    .id(SharedString::from(format!("board-status-{id}")))
                    .role(gpui::Role::MenuItem)
                    .aria_label(SharedString::from(text.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board_move_ticket(work_item_id.clone(), to, cx)
                    }))
                    .child(icon(glyph).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from(text))
                    .relative()
                    .children(self.board_probe_child(|| probe.clone())),
            );
        }
        let open_item = menu.work_item_id.clone();
        card = card
            .child(popover::menu_separator())
            .child(
                popover::menu_row(&theme, false, "board-status-open")
                    .id("board-status-open")
                    .role(gpui::Role::MenuItem)
                    .aria_label(SharedString::from(format!("Open ticket {}", badge.key)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chat_open_ticket(open_item.clone(), cx)
                    }))
                    .child(
                        icon(icons::CHAT_TICKET)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from(format!("Open ticket {}", badge.key))),
            )
            .child(
                div()
                    .px(crate::typography::ui_rems(8.0))
                    .pt(crate::typography::ui_rems(4.0))
                    .pb(crate::typography::ui_rems(2.0))
                    .whitespace_normal()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.readable_muted(0.6))
                    .child("Done is set when the current checks and review pass."),
            );
        Some(popover::menu_at(
            "board-status-menu",
            menu.position,
            card.into_any_element(),
            None,
        ))
    }
}

/// One board menu row: id, glyph, words, why it is unavailable (when it is
/// shown only to explain), and what it does.
type MenuRow = (
    &'static str,
    &'static str,
    String,
    Option<String>,
    Option<RowAction>,
);

/// What a board menu row does.
#[derive(Clone)]
enum RowAction {
    OpenTicket(String),
    Dialog(DialogKind),
    Sibling,
}
