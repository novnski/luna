//! The chat's board actions: Convert this chat to a ticket, Create ticket
//! from this chat, Hand off to a new main chat (dialogs), New side chat (a
//! fresh sibling) and the side chat's creation card that replaces the main
//! chat's Fresh / With context chooser.
//!
//! Each intent keeps one idempotency key: a lost answer is checked by
//! sending the very same request again (the engine returns its receipt),
//! never by starting a second one.
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Point,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use luna_proto::{
    BoardChatRole, BoardContextMode, BoardConvertChatRequest, BoardCreateMainChatRequest,
    BoardCreateRequest, BoardCreateSideChatRequest, BoardItemOrigin, BoardPhase, BoardPlacement,
    ChatTicketBadge, TaskLinkRole,
};

use super::super::api::BoardCallError;
use super::{ChatReceipt, ReceiptKind, new_request_id};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::interaction::HoverFill as _;
use crate::popover;
use crate::shell::{Shell, SidebarNotice};
use crate::theme::Theme;

/// The handoff brief's sections (chat-model §2: goal, completed work,
/// files/artifacts, decisions, constraints, next step).
const HANDOFF_TEMPLATE: &str =
    "Goal:\nCompleted so far:\nFiles and artifacts:\nDecisions:\nConstraints:\nNext step:";

/// The longest proposed brief taken from the conversation.
const BRIEF_PREFILL_MAX: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogKind {
    /// Create a ticket and link this same chat as its working chat.
    Convert,
    /// Save a ticket with this chat as its source; no chat is created.
    CreateTicket,
    /// From a side chat: a distinct new main chat with a written brief.
    Handoff,
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            Self::Convert => "Convert this chat to a ticket",
            Self::CreateTicket => "Create ticket from this chat",
            Self::Handoff => "Hand off to a new main chat",
        }
    }

    fn explanation(self) -> &'static str {
        match self {
            Self::Convert => {
                "This chat becomes the ticket's working chat. It keeps its history, provider, settings, placement and any run in progress; nothing is sent again."
            }
            Self::CreateTicket => {
                "Saves a ticket in Backlog with this chat as its source. No chat is created and nothing starts."
            }
            Self::Handoff => {
                "Creates a separate chat in the sidebar with this written brief. It does not copy this conversation, and this side chat stays where it is."
            }
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Convert => "convert",
            Self::CreateTicket => "create",
            Self::Handoff => "handoff",
        }
    }
}

pub(crate) struct BoardChatDialog {
    pub(crate) kind: DialogKind,
    pub(crate) chat_id: String,
    pub(crate) project_id: Option<String>,
    pub(crate) title: Entity<ComposerInput>,
    pub(crate) body: Entity<ComposerInput>,
    /// The request in flight or with an unknown outcome: `(id, start)`.
    pub(crate) request: Option<(String, bool)>,
    pub(crate) busy: bool,
    pub(crate) error: Option<SharedString>,
    /// The last answer was lost: the primary action checks that request.
    pub(crate) lost: bool,
    focus_pending: bool,
    _events: Subscription,
}

/// The side chat's creation card (its `+`): New side chat or Hand off.
pub(crate) struct SideOriginCard {
    /// The workbench group whose `+` opened it (the keyboard returns there).
    pub(crate) pane_id: Option<String>,
    pub(crate) side_id: String,
    pub(crate) main_id: Option<String>,
    pub(crate) anchor: Point<Pixels>,
    pub(crate) focus: FocusHandle,
    focus_pending: bool,
}

/// A fresh sibling in flight; its key survives an unknown outcome.
pub(crate) struct SiblingCreate {
    pub(crate) side_id: String,
    pub(crate) request_id: String,
    pub(crate) busy: bool,
    _task: Option<gpui::Task<()>>,
}

fn single_line(text: &str) -> String {
    crate::transcript::single_line(text)
}

impl Shell {
    fn board_chat_title(&self, chat_id: &str, cx: &gpui::App) -> String {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.title.as_deref())
            .map(single_line)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled chat".into())
    }

    /// A starting brief from the conversation: the latest request in it
    /// (the user edits it; prior messages are never rewritten).
    fn board_chat_brief(&self, chat_id: &str, cx: &gpui::App) -> String {
        let state = self.state.read(cx);
        let text = crate::chat_binding::ChatBinding::Chat(chat_id.to_string())
            .transcript(state)
            .iter()
            .rev()
            .find(|entry| entry.role == luna_doc::MessageRole::User)
            .map(|entry| {
                entry
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        luna_doc::MessagePart::Text { text, .. } => Some(text.trim()),
                        _ => None,
                    })
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        text.chars().take(BRIEF_PREFILL_MAX).collect()
    }

    /// Open one of the chat's board dialogs, proposing the chat's title.
    pub(crate) fn board_open_dialog(
        &mut self,
        kind: DialogKind,
        chat_id: String,
        cx: &mut Context<Self>,
    ) {
        self.board_chat.side_card = None;
        self.board_chat.status_menu = None;
        let chat_title = self.board_chat_title(&chat_id, cx);
        let project_id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.space_id.clone());
        let proposed_title = match kind {
            DialogKind::Convert => chat_title.clone(),
            DialogKind::CreateTicket => format!("Follow-up: {chat_title}"),
            DialogKind::Handoff => format!("Continue: {chat_title}"),
        };
        let brief = match kind {
            DialogKind::Handoff => HANDOFF_TEMPLATE.to_string(),
            _ => self.board_chat_brief(&chat_id, cx),
        };
        let title = cx.new(|cx| {
            ComposerInput::new("Title", cx)
                .with_single_line()
                .with_accessibility_role(gpui::Role::TextInput)
        });
        title.update(cx, |input, cx| input.set_text(proposed_title, cx));
        let body = cx.new(|cx| {
            ComposerInput::new(
                match kind {
                    DialogKind::Handoff => "The written brief for the new chat",
                    _ => "The agreed brief",
                },
                cx,
            )
            .with_accessibility_role(gpui::Role::TextInput)
        });
        body.update(cx, |input, cx| input.set_text(brief, cx));
        let events = cx.subscribe(&title, |shell: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                shell.board_dialog_submit(false, cx);
            }
        });
        self.board_chat.dialog = Some(BoardChatDialog {
            kind,
            chat_id,
            project_id,
            title,
            body,
            request: None,
            busy: false,
            error: None,
            lost: false,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(crate) fn board_dialog_cancel(&mut self, cx: &mut Context<Self>) {
        if self
            .board_chat
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.busy)
        {
            return;
        }
        self.board_chat.dialog = None;
        cx.notify();
    }

    /// The dialog's primary action (`start` only for Hand off: start
    /// working there instead of only preparing the chat).
    pub(crate) fn board_dialog_submit(&mut self, start: bool, cx: &mut Context<Self>) {
        let Some(api) = self.board_chat_api(cx) else {
            if let Some(dialog) = self.board_chat.dialog.as_mut() {
                dialog.error = Some("Engine not connected.".into());
            }
            cx.notify();
            return;
        };
        let Some(dialog) = self.board_chat.dialog.as_mut() else {
            return;
        };
        if dialog.busy {
            return;
        }
        let title = dialog.title.read(cx).text().trim().to_string();
        let body = dialog.body.read(cx).text().trim().to_string();
        // A lost answer repeats the same request (same key, same payload);
        // anything else is a new intent with a new key.
        let start = if dialog.lost {
            dialog
                .request
                .as_ref()
                .map(|(_, start)| *start)
                .unwrap_or(start)
        } else {
            start
        };
        let request_id = match (&dialog.request, dialog.lost) {
            (Some((id, sent_start)), true) if *sent_start == start => id.clone(),
            _ => new_request_id(&format!("chat-{}", dialog.kind.key())),
        };
        let kind = dialog.kind;
        let chat_id = dialog.chat_id.clone();
        let project_id = dialog.project_id.clone();
        match kind {
            DialogKind::CreateTicket | DialogKind::Handoff if title.is_empty() => {
                dialog.error = Some("Give it a title.".into());
                cx.notify();
                return;
            }
            DialogKind::Handoff if body.is_empty() || body == HANDOFF_TEMPLATE => {
                dialog.error = Some(
                    "Write the brief: the remaining goal, what is done, the relevant files, decisions, constraints and the next step."
                        .into(),
                );
                cx.notify();
                return;
            }
            _ => {}
        }
        let Some(project_id) = project_id.filter(|id| !id.is_empty()) else {
            dialog.error = Some("This chat has no project, so it cannot hold a ticket.".into());
            cx.notify();
            return;
        };
        dialog.request = Some((request_id.clone(), start));
        dialog.busy = true;
        dialog.error = None;
        dialog.lost = false;
        let generation = self.state.read(cx).engine_generation();
        let harness = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.config.as_ref())
            .map(|config| config.harness);
        self.board_chat.op = Some(cx.spawn(async move |this, cx| {
            let outcome: Result<DialogOutcome, BoardCallError> = match kind {
                DialogKind::Convert => api
                    .convert_chat(BoardConvertChatRequest {
                        request_id,
                        task_id: chat_id.clone(),
                        title: (!title.is_empty()).then_some(title),
                        description: body,
                        implementation: None,
                        review: None,
                        completion: None,
                    })
                    .await
                    .map(|receipt| DialogOutcome::Converted(Box::new(receipt))),
                DialogKind::CreateTicket => api
                    .create(BoardCreateRequest {
                        request_id,
                        project_id: Some(project_id),
                        title,
                        description: body,
                        implementation: None,
                        review: None,
                        completion: None,
                        labels: Vec::new(),
                        priority: None,
                        context_refs: Vec::new(),
                        attachments: Vec::new(),
                        input_items: Vec::new(),
                        origin: Some(BoardItemOrigin::Chat {
                            task_id: chat_id.clone(),
                            message_id: None,
                        }),
                        placement: BoardPlacement::Top,
                    })
                    .await
                    .map(|receipt| DialogOutcome::Created(Box::new(receipt))),
                DialogKind::Handoff => api
                    .create_main_chat(BoardCreateMainChatRequest {
                        request_id,
                        project_id,
                        from_task_id: Some(chat_id.clone()),
                        handoff_brief: Some(body),
                        title: Some(title.clone()),
                        start,
                        ticket: None,
                        execution: None,
                    })
                    .await
                    .map(|created| DialogOutcome::HandedOff(Box::new(created), title)),
            };
            this.update(cx, |shell, cx| {
                shell.board_dialog_finished(generation, chat_id, harness, outcome, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn board_dialog_finished(
        &mut self,
        generation: u64,
        chat_id: String,
        harness: Option<luna_proto::HarnessId>,
        outcome: Result<DialogOutcome, BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        self.board_chat.op = None;
        let current = self
            .board_chat
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.chat_id == chat_id);
        match outcome {
            Ok(outcome) => {
                if current {
                    self.board_chat.dialog = None;
                }
                if self.state.read(cx).engine_generation() != generation {
                    self.board_chat_refresh(cx);
                    cx.notify();
                    return;
                }
                match outcome {
                    DialogOutcome::Converted(receipt) => {
                        let mutation = &receipt.mutation;
                        // The engine's answer is canonical: show its badge and
                        // identity now; the badge read that follows confirms.
                        self.board_chat.badges.insert(
                            chat_id.clone(),
                            ChatTicketBadge {
                                task_id: chat_id.clone(),
                                work_item_id: mutation.work_item_id.clone(),
                                key: mutation.key.clone(),
                                title: mutation.item.title.clone(),
                                column: mutation.column,
                                phase: BoardPhase::Unknown,
                                role: TaskLinkRole::Working,
                                harness,
                            },
                        );
                        self.board_chat
                            .identities
                            .insert(chat_id.clone(), receipt.identity.clone());
                        self.board_chat.push_receipt(
                            &chat_id,
                            ChatReceipt {
                                kind: ReceiptKind::Converted,
                                work_item_id: Some(mutation.work_item_id.clone()),
                                key: Some(mutation.key.clone()),
                                title: mutation.item.title.clone(),
                                column: Some(mutation.column),
                            },
                        );
                        if let Some(model) = self.board_chat.model.clone() {
                            model.update(cx, |model, cx| model.apply_receipt(mutation, cx));
                        }
                        self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                            "{} created. This chat is its working chat.",
                            mutation.key
                        )));
                    }
                    DialogOutcome::Created(receipt) => {
                        let related = self.board_chat.related.entry(chat_id.clone()).or_default();
                        if !related
                            .iter()
                            .any(|badge| badge.work_item_id == receipt.work_item_id)
                        {
                            related.push(ChatTicketBadge {
                                task_id: chat_id.clone(),
                                work_item_id: receipt.work_item_id.clone(),
                                key: receipt.key.clone(),
                                title: receipt.item.title.clone(),
                                column: receipt.column,
                                phase: BoardPhase::NotStarted,
                                role: TaskLinkRole::Source,
                                harness: None,
                            });
                        }
                        self.board_chat.push_receipt(
                            &chat_id,
                            ChatReceipt {
                                kind: ReceiptKind::Created,
                                work_item_id: Some(receipt.work_item_id.clone()),
                                key: Some(receipt.key.clone()),
                                title: receipt.item.title.clone(),
                                column: Some(receipt.column),
                            },
                        );
                        if let Some(model) = self.board_chat.model.clone() {
                            model.update(cx, |model, cx| model.apply_receipt(&receipt, cx));
                        }
                        self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                            "{} saved to {}. No chat was created.",
                            receipt.key,
                            receipt.column.label()
                        )));
                    }
                    DialogOutcome::HandedOff(created, title) => {
                        let started = created.launch != luna_proto::BoardChatLaunchState::Created;
                        self.board_chat.push_receipt(
                            &chat_id,
                            ChatReceipt {
                                kind: ReceiptKind::HandedOff {
                                    task_id: created.task_id.clone(),
                                    started,
                                },
                                work_item_id: created.work_item_id.clone(),
                                key: None,
                                title,
                                column: None,
                            },
                        );
                        if started {
                            self.board_open_navigation(created.navigation, cx);
                        } else {
                            self.sidebar_notice = Some(SidebarNotice::transient_status(
                                "The new main chat is ready with your brief. This side chat stays here.",
                            ));
                        }
                    }
                }
                self.board_chat_refresh(cx);
            }
            Err(error) => {
                if let Some(dialog) = self.board_chat.dialog.as_mut().filter(|_| current) {
                    dialog.busy = false;
                    dialog.lost = matches!(error, BoardCallError::Lost);
                    if !dialog.lost {
                        dialog.request = None;
                    }
                    dialog.error = Some(error.message().into());
                } else {
                    self.board_chat_notice_error(error.message(), cx);
                }
            }
        }
        cx.notify();
    }

    /// New side chat from a side chat: a fresh sibling under the same main
    /// chat (the engine resolves that main parent; the caller stays put).
    pub(crate) fn board_create_sibling(&mut self, side_id: String, cx: &mut Context<Self>) {
        self.board_chat.side_card = None;
        if self
            .board_chat
            .sibling
            .as_ref()
            .is_some_and(|sibling| sibling.busy)
        {
            return;
        }
        let Some(api) = self.board_chat_api(cx) else {
            self.board_chat_notice_error("Engine not connected.", cx);
            return;
        };
        // An earlier create from this side chat with an unknown outcome is
        // repeated as the same request.
        let request_id = self
            .board_chat
            .sibling
            .as_ref()
            .filter(|sibling| sibling.side_id == side_id)
            .map(|sibling| sibling.request_id.clone())
            .unwrap_or_else(|| new_request_id("chat-sibling"));
        let request = BoardCreateSideChatRequest {
            request_id: request_id.clone(),
            caller_task_id: side_id.clone(),
            context: BoardContextMode::Fresh,
            title: None,
            prompt: None,
            start: false,
            ticket: None,
            execution: None,
        };
        let task = cx.spawn(async move |this, cx| {
            let result = api.create_side_chat(request).await;
            this.update(cx, |shell, cx| {
                let lost = matches!(result, Err(BoardCallError::Lost));
                match result {
                    Ok(created) => {
                        shell.board_chat.sibling = None;
                        shell.board_open_navigation(created.navigation, cx);
                    }
                    Err(error) => {
                        if lost {
                            if let Some(sibling) = shell.board_chat.sibling.as_mut() {
                                sibling.busy = false;
                                sibling._task = None;
                            }
                            shell.board_chat_notice_error(
                                "The new side chat may already exist. Choose New side chat again to check; it will not make a second one.",
                                cx,
                            );
                        } else {
                            shell.board_chat.sibling = None;
                            shell.board_chat_notice_error(error.message(), cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.board_chat.sibling = Some(SiblingCreate {
            side_id,
            request_id,
            busy: true,
            _task: Some(task),
        });
        self.sidebar_notice = Some(SidebarNotice::transient_status("Creating side chat…"));
        cx.notify();
    }

    /// A side chat's `+`: its creation card instead of the main chat's
    /// Fresh / With context chooser. Returns false for a main chat (the
    /// ordinary chooser opens).
    pub(in crate::shell) fn board_side_origin_card(
        &mut self,
        pane_id: Option<&str>,
        source: &str,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.board_chat_ensure_identity(source, cx);
        let role = self.board_chat_role(source, cx);
        if role.role != BoardChatRole::Side {
            return false;
        }
        self.board_chat.status_menu = None;
        self.board_chat.side_card = Some(SideOriginCard {
            pane_id: pane_id.map(str::to_string),
            side_id: source.to_string(),
            main_id: role.main_parent_id,
            anchor,
            focus: cx.focus_handle(),
            focus_pending: true,
        });
        cx.notify();
        true
    }

    /// Close the card without creating anything. Escape returns the
    /// keyboard to the side chat in the group it was opened from (as the
    /// chooser's Escape does); a press elsewhere leaves focus where it went.
    fn board_close_side_card(&mut self, restore_focus: bool, cx: &mut Context<Self>) {
        if let Some(card) = self.board_chat.side_card.take() {
            if restore_focus && let Some(pane_id) = card.pane_id.as_deref() {
                self.workbench_focus_group(pane_id, Some(&card.side_id), cx);
            }
            cx.notify();
        }
    }

    fn board_side_card_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt {
            return false;
        }
        let Some(side) = self
            .board_chat
            .side_card
            .as_ref()
            .map(|card| card.side_id.clone())
        else {
            return false;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.board_close_side_card(true, cx),
            "enter" | "n" => self.board_create_sibling(side, cx),
            "h" => {
                self.board_chat.side_card = None;
                self.board_open_dialog(DialogKind::Handoff, side, cx);
            }
            _ => return false,
        }
        true
    }

    /// This slice's overlays: the side chat's creation card, the status
    /// menu and the board dialog.
    pub(in crate::shell) fn render_board_chat_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut overlays = Vec::new();
        if let Some(card) = self.render_board_side_card(window, cx) {
            overlays.push(card);
        }
        if let Some(menu) = self.render_board_status_menu(cx) {
            overlays.push(menu);
        }
        if let Some(dialog) = self.render_board_dialog(viewport, window, cx) {
            overlays.push(dialog);
        }
        overlays
    }

    fn render_board_side_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let card = self.board_chat.side_card.as_mut()?;
        if std::mem::take(&mut card.focus_pending) {
            window.focus(&card.focus, cx);
        }
        let side = card.side_id.clone();
        let main = card.main_id.clone();
        let anchor = card.anchor;
        let focus = card.focus.clone();
        let theme = Theme::of(cx).for_popup();
        let main_title = main
            .as_deref()
            .map(|main| self.board_chat_title(main, cx))
            .unwrap_or_else(|| "its main chat".into());
        let side_title = self.board_chat_title(&side, cx);
        let muted = theme.readable_muted(0.7);
        let option = |id: &'static str,
                      glyph: &'static str,
                      title: &'static str,
                      body: String,
                      key: &'static str| {
            div()
                .id(id)
                .role(gpui::Role::MenuItem)
                .aria_label(SharedString::from(format!("{title}: {body}")))
                .tab_index(0)
                .relative()
                .p(crate::typography::ui_rems(8.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover_wash(id)
                .flex()
                .flex_row()
                .items_start()
                .gap(crate::typography::ui_rems(8.0))
                .child(
                    icon(glyph)
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
                                .flex()
                                .flex_row()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(muted)
                                        .child(key),
                                ),
                        )
                        .child(
                            div()
                                .whitespace_normal()
                                .text_size(crate::typography::ui_rems(11.0))
                                .line_height(crate::typography::ui_rems(15.0))
                                .text_color(muted)
                                .child(SharedString::from(body)),
                        ),
                )
        };
        let sibling_side = side.clone();
        let handoff_side = side.clone();
        let sibling = option(
            "board-side-card-new",
            icons::PLUS,
            "New side chat",
            format!(
                "Fresh, under {main_title}. This side chat's conversation is not carried over."
            ),
            "N",
        )
        .on_click(
            cx.listener(move |this, _, _, cx| this.board_create_sibling(sibling_side.clone(), cx)),
        )
        .children(self.board_probe_child(|| "side-card:new".into()));
        let handoff = option(
            "board-side-card-handoff",
            icons::ARROW_UP_RIGHT,
            "Hand off to a new main chat…",
            "A separate chat in the sidebar with a written brief. This side chat stays here."
                .to_string(),
            "H",
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.board_chat.side_card = None;
            this.board_open_dialog(DialogKind::Handoff, handoff_side.clone(), cx)
        }))
        .children(self.board_probe_child(|| "side-card:handoff".into()));
        self.board_mark(
            || "side-card".into(),
            || {
                vec![
                    "New side chat".into(),
                    "Hand off to a new main chat…".into(),
                ]
            },
        );
        let card = popover::popover_card(&theme)
            .id("board-side-card")
            .track_focus(&focus)
            .bg(theme.tooltip_surface())
            .w(crate::typography::ui_rems(320.0))
            .whitespace_normal()
            .p(crate::typography::ui_rems(12.0))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.board_close_side_card(false, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.board_side_card_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(muted)
                            .child("From the side chat"),
                    )
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .truncate()
                            .child(SharedString::from(side_title)),
                    ),
            )
            .child(sibling)
            .child(handoff)
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(crate::typography::ui_rems(15.0))
                    .text_color(muted)
                    .child(
                        "Side chats stay one level deep: they can't fork, carry their context, or hold side chats of their own.",
                    ),
            );
        Some(popover::menu_at(
            "board-side-card-menu",
            anchor,
            card.into_any_element(),
            None,
        ))
    }

    fn render_board_dialog(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.board_chat.dialog.as_mut()?;
        if std::mem::take(&mut dialog.focus_pending) {
            window.focus(&dialog.title.focus_handle(cx), cx);
        }
        let kind = dialog.kind;
        let title = dialog.title.clone();
        let body = dialog.body.clone();
        let busy = dialog.busy;
        let lost = dialog.lost;
        let error = dialog.error.clone();
        let theme = Theme::of(cx).for_popup();
        let field_label = |text: &'static str| {
            div()
                .mt(px(12.0))
                .mb(px(4.0))
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text_muted)
                .child(text)
        };
        let primary_label = match (kind, lost) {
            (_, true) => "Check status",
            (DialogKind::Convert, false) => "Convert",
            (DialogKind::CreateTicket, false) => "Save ticket",
            (DialogKind::Handoff, false) => "Start working there",
        };
        let mut buttons: Vec<AnyElement> = vec![
            popover::btn_ghost(&theme, "Cancel", "board-dialog-cancel")
                .id("board-dialog-cancel")
                .on_click(cx.listener(|this, _, _, cx| this.board_dialog_cancel(cx)))
                .relative()
                .children(self.board_probe_child(|| "dialog:cancel".into()))
                .into_any_element(),
        ];
        if kind == DialogKind::Handoff && !lost {
            buttons.push(
                popover::btn_ghost(&theme, "Prepare chat", "board-dialog-prepare")
                    .id("board-dialog-prepare")
                    .on_click(cx.listener(|this, _, _, cx| this.board_dialog_submit(false, cx)))
                    .when(busy, |el| el.opacity(0.5))
                    .relative()
                    .children(self.board_probe_child(|| "dialog:prepare".into()))
                    .into_any_element(),
            );
        }
        let start = kind == DialogKind::Handoff;
        buttons.push(
            popover::btn_primary(&theme, if busy { "Working…" } else { primary_label })
                .id("board-dialog-primary")
                .on_click(cx.listener(move |this, _, _, cx| this.board_dialog_submit(start, cx)))
                .when(busy, |el| el.opacity(0.5))
                .relative()
                .children(self.board_probe_child(|| "dialog:primary".into()))
                .into_any_element(),
        );
        self.board_mark(
            || "dialog".into(),
            || {
                let mut marks = vec![kind.title().to_string(), primary_label.to_string()];
                if let Some(error) = &error {
                    marks.push(format!("error:{error}"));
                }
                marks
            },
        );
        let card = popover::dialog_card(&theme)
            .w(crate::typography::ui_rems(440.0))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.board_dialog_cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        icon(match kind {
                            DialogKind::Handoff => icons::ARROW_UP_RIGHT,
                            DialogKind::CreateTicket => icons::CLIPBOARD_LIST,
                            DialogKind::Convert => icons::CHAT_TICKET,
                        })
                        .size(px(16.0))
                        .text_color(theme.text_muted),
                    )
                    .child(popover::dialog_title(&theme, kind.title())),
            )
            .child(
                div()
                    .mt(px(8.0))
                    .child(popover::dialog_body(&theme, kind.explanation())),
            )
            .child(field_label("Title"))
            .child(
                div()
                    .relative()
                    .child(popover::dialog_field(title.into_any_element()))
                    .children(self.board_probe_child(|| "dialog:title".into())),
            )
            .child(field_label(match kind {
                DialogKind::Handoff => "Handoff brief",
                _ => "Brief",
            }))
            .child(
                div()
                    .relative()
                    .max_h(crate::typography::ui_rems(180.0))
                    .overflow_hidden()
                    .child(popover::dialog_field(body.into_any_element()))
                    .children(self.board_probe_child(|| "dialog:body".into())),
            )
            .when_some(error, |el, error| {
                el.child(
                    div()
                        .mt(px(10.0))
                        .flex()
                        .flex_row()
                        .gap(px(6.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.readable_tone(theme.danger, 1.0))
                        .child(
                            icon(icons::DANGER_TRIANGLE)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(theme.readable_icon(theme.danger)),
                        )
                        .child(div().whitespace_normal().child(error)),
                )
            })
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .children(buttons),
            )
            .into_any_element();
        Some(popover::modal("board-chat-dialog", viewport, card))
    }
}

/// What a board dialog's request returned.
enum DialogOutcome {
    Converted(Box<luna_proto::BoardConvertReceipt>),
    Created(Box<luna_proto::BoardMutationReceipt>),
    HandedOff(Box<luna_proto::BoardChatCreated>, String),
}
