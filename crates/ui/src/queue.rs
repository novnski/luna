//! The pending-message queue, docked above the composer.
//!
//! Everything you typed while the agent was busy, in the order it will be sent.
//! The rows live on the session doc ([`luna_doc::QueuedMessage`]), so the phone
//! shows the same queue and either device can reorder it.
//!
//! Each row exposes either explicit `Steer now` (when the running harness can
//! accept it) or `Send now`, which interrupts the active response.
//! Editing moves the message into the composer while its leased row reserves
//! its position.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, prelude::*, px,
};
use sha2::{Digest, Sha256};

use luna_doc::{QueueDeliveryGate, QueueHoldReason, QueueMessageStatus, QueuedMessage};
use luna_rpc::methods;

use crate::composer::{Composer, QUEUE_COMPOSER_OVERLAP};
use crate::icons::{self, icon};
use crate::motion::{self, AnimationExt as _, TAB_SLIDE};
use crate::terminal::panel::{drop_index, slide_offset};
use crate::theme::Theme;

/// Mutations of existing queue rows must reach the owning device. A stale
/// replica can still show Held after the owner has claimed delivery, so its
/// local snapshot cannot authorize an edit or reorder safely.
fn queue_action_needs_host(method: &str) -> bool {
    matches!(
        method,
        methods::SEND_QUEUED_MESSAGE_NOW
            | methods::STEER_QUEUED_MESSAGE_NOW
            | methods::REMOVE_QUEUED_MESSAGE
            | methods::BEGIN_QUEUED_MESSAGE_EDIT
            | methods::RENEW_QUEUED_MESSAGE_EDIT
            | methods::FINISH_QUEUED_MESSAGE_EDIT
            | methods::UPDATE_QUEUED_MESSAGE
            | methods::MOVE_QUEUED_MESSAGE
    )
}

/// Queue mutation replies are deliberately explicit. A false or malformed
/// acknowledgement means an optimistic local edit may not match the document
/// (for example, another device removed the same row first).
fn queue_mutation_acknowledged(method: &str, reply: &serde_json::Value) -> bool {
    let field = match method {
        methods::UPDATE_QUEUED_MESSAGE | methods::MOVE_QUEUED_MESSAGE => "changed",
        methods::REMOVE_QUEUED_MESSAGE => "removed",
        methods::SEND_QUEUED_MESSAGE_NOW | methods::STEER_QUEUED_MESSAGE_NOW => "sent",
        _ => return true,
    };
    reply.get(field).and_then(serde_json::Value::as_bool) == Some(true)
}

/// Stable retry identity for one attachment in one queue-edit command. The
/// upload jail caps ids at 64 safe ASCII bytes; concatenating two UUIDs with a
/// descriptive prefix exceeded that boundary in native QA.
fn queue_edit_upload_id(command_id: &str, attachment_id: &str) -> String {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{command_id}\0{attachment_id}").as_bytes())
    );
    format!("qe-{}", &digest[..40])
}

struct QueueActionTooltip {
    label: SharedString,
}

impl Render for QueueActionTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let card = div()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(theme))
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_muted)
            .child(self.label.clone());
        crate::frost::frosted(6.0, crate::frost::MENU_BLUR, card)
    }
}

/// Compact, borderless rows inside the queue's single glass surface.
const ROW_HEIGHT: f32 = 36.0;
const QUEUE_TEXT_SIZE: f32 = 12.5;
const ROW_GAP: f32 = 0.0;
const ROW_SLOT: f32 = ROW_HEIGHT + ROW_GAP;
const ROW_PAD_X: f32 = 8.0;
const ROW_RADIUS: f32 = 8.0;
const PANEL_RADIUS: f32 = 16.0;
const PANEL_PAD_TOP: f32 = 0.0;
/// The custom 24px queue glyphs have quieter geometry than the legacy set, so
/// render them slightly larger to preserve the previous optical weight.
const QUEUE_ICON_SIZE: f32 = 13.0;

/// The single trailing action a queue row advertises and executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePrimaryAction {
    SendNow,
    SteerNow,
}

impl QueuePrimaryAction {
    fn tooltip(self) -> &'static str {
        match self {
            Self::SendNow => "Send now (interrupts an active response)",
            Self::SteerNow => "Steer now (without interrupting)",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SendNow => "Send now",
            Self::SteerNow => "Steer now",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePrimaryBlock {
    HostUnsupported,
    Removing,
    Editing,
    ReviewRequired,
    DeliveryPending,
    Claimed,
    SteeringAccepted,
    DeliveryUnknown,
}

impl QueuePrimaryBlock {
    fn tooltip(self) -> &'static str {
        match self {
            Self::HostUnsupported => "The chat host does not support queue actions",
            Self::Removing => "Removal is already in progress",
            Self::Editing => "Finish editing before sending",
            Self::ReviewRequired => "Review this message before sending",
            Self::DeliveryPending => "A delivery is already in progress",
            Self::Claimed => "This message is already being delivered",
            Self::SteeringAccepted => {
                "Accepted and pending native recording; model receipt is unconfirmed"
            }
            Self::DeliveryUnknown => "Delivery outcome is unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueuePrimaryResolution {
    action: QueuePrimaryAction,
    blocked: Option<QueuePrimaryBlock>,
}

impl QueuePrimaryResolution {
    fn enabled(self) -> bool {
        self.blocked.is_none()
    }

    fn tooltip(self) -> &'static str {
        self.blocked
            .map(QueuePrimaryBlock::tooltip)
            .unwrap_or_else(|| self.action.tooltip())
    }
}

fn queue_hold_reason_label(reason: QueueHoldReason) -> &'static str {
    match reason {
        QueueHoldReason::SteeringUnavailable => "Steer unavailable",
        QueueHoldReason::AttachmentsUnsupported => "Attachments cannot be steered",
        QueueHoldReason::AwaitingApproval => "Waiting for approval",
        QueueHoldReason::AwaitingInput => "Waiting for an answer",
        QueueHoldReason::StaleTurn => "The active turn changed",
        QueueHoldReason::ProviderRejected => "Provider rejected steer",
        QueueHoldReason::ContextChanged => "Review after context change",
        QueueHoldReason::RemoteAuthorityRequired => "Remote message needs local review",
    }
}

/// Resolve the one explicit action this row advertises. Native Steer is
/// deliberately narrow; outside that safe boundary, a Held row still exposes
/// Send now as a user-authorized override. Submitting the original Steer never
/// reaches this resolver, so there is no automatic interrupt/fresh-turn
/// fallback. The engine owns the revision/turn/binding fences.
fn resolve_queue_primary_action(
    behavior: luna_proto::FollowUpBehavior,
    run_live: bool,
    harness_steers_mid_turn: bool,
    has_attachments: bool,
    has_hold_reason: bool,
    blocked: Option<QueuePrimaryBlock>,
) -> QueuePrimaryResolution {
    let action = match behavior {
        luna_proto::FollowUpBehavior::Queue => QueuePrimaryAction::SendNow,
        luna_proto::FollowUpBehavior::Steer
            if run_live && harness_steers_mid_turn && !has_attachments && !has_hold_reason =>
        {
            QueuePrimaryAction::SteerNow
        }
        luna_proto::FollowUpBehavior::Steer => QueuePrimaryAction::SendNow,
    };
    QueuePrimaryResolution { action, blocked }
}

/// Translate a pointer inside the whole panel into a row slot. The top pad
/// belongs to slot zero; the bottom pad clamps to the final row.
fn queue_drop_index(panel_y: f32, count: usize) -> usize {
    drop_index(panel_y - PANEL_PAD_TOP, ROW_SLOT, count)
}

/// Paint-only start and target positions for the PR #90 reorder treatment.
/// The dragged row travels to the hovered slot while every row in its path
/// slides into the space it leaves behind.
fn queue_drag_offsets(ix: usize, from: usize, prev_over: usize, over: usize) -> (f32, f32) {
    if ix == from {
        (
            (prev_over as f32 - from as f32) * ROW_SLOT,
            (over as f32 - from as f32) * ROW_SLOT,
        )
    } else {
        (
            slide_offset(ix, from, prev_over) * ROW_SLOT,
            slide_offset(ix, from, over) * ROW_SLOT,
        )
    }
}

/// A queue row being dragged (gpui drag-and-drop). Scoped to its chat so a
/// drag can't land in a queue it didn't come from.
pub struct QueueDragPayload {
    chat: String,
    from: usize,
}

/// Where the dragged row would land, including the previous slot needed to
/// restart the short PR #90-style slide from its current visual position.
pub struct QueueDragState {
    pub from: usize,
    pub over: usize,
    pub prev_over: usize,
    pub epoch: usize,
}

/// Invisible cursor ghost: the real row stays in the queue and moves between
/// slots, instead of following the pointer as a detached tooltip.
struct QueueGhost;

impl Render for QueueGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// One line of a queued message: the newlines that make it a paragraph in the
/// composer make it three rows here, and the row is one line tall.
fn one_line(text: &str) -> SharedString {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    SharedString::from(flat)
}

/// New queue rows contain only editable user text. During a rolling upgrade,
/// an older client may still have stored the attachment trailer in `text`.
/// Hide it only when the parsed paths exactly match the row's attachment field.
fn queue_visible_text(text: &str, attachments: &[String]) -> String {
    let text = crate::appshots::strip_context_for_display(text);
    if text.trim().is_empty() && !attachments.is_empty() {
        return crate::attachments::ATTACHMENT_ONLY_TEXT.to_string();
    }
    if attachments.is_empty() {
        return text.to_string();
    }
    let parsed = crate::attachments::parse_user_message_images(text);
    let paths_match = parsed.attachments.len() == attachments.len()
        && parsed
            .attachments
            .iter()
            .zip(attachments)
            .all(|(parsed, stored)| parsed.path == *stored);
    if !paths_match {
        return text.to_string();
    }
    if parsed.text.trim().is_empty() {
        crate::attachments::ATTACHMENT_ONLY_TEXT.to_string()
    } else {
        parsed.text
    }
}

/// Presentation-only metadata. Never expose the observed accessibility payload.
fn queue_attachment_labels(text: &str, paths: &[String]) -> Vec<String> {
    let presentations = crate::appshots::presentations(text);
    paths
        .iter()
        .map(|path| match presentations.get(path) {
            Some(appshot) => format!("{} Appshot", appshot.app_name),
            None => std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Image")
                .to_owned(),
        })
        .collect()
}

fn queue_panel_surface(theme: &Theme) -> gpui::Div {
    div()
        .occlude()
        .rounded_t(px(PANEL_RADIUS))
        .bg(crate::popover::surface_bg(theme))
        .border_1()
        .border_color(theme.border)
        .when(!theme.is_frost(), |el| el.shadow_lg())
        // Keep visible rows flush with the tray; only the portion tucked behind
        // the composer needs padding.
        .pb(px(QUEUE_COMPOSER_OVERLAP))
        .flex()
        .flex_col()
}

fn queue_rows(
    scroll: &gpui::ScrollHandle,
    max_height: gpui::Pixels,
    rows: impl IntoIterator<Item = AnyElement>,
) -> crate::edge_fade::EdgeFaded {
    crate::edge_fade::edge_faded(
        Theme::TRANSCRIPT_FADE_BAND,
        true,
        true,
        div()
            .id("message-queue-rows")
            .max_h(max_height)
            .overflow_y_scroll()
            .track_scroll(scroll)
            .flex()
            .flex_col()
            .gap(px(ROW_GAP))
            .children(rows),
    )
    .fade_overflow_y(scroll)
    // GPUI samples glyph fades at baseline + font size.
    .outset_bottom(QUEUE_TEXT_SIZE)
}

fn preview_load_gate() -> &'static futures::lock::Mutex<()> {
    static GATE: std::sync::OnceLock<futures::lock::Mutex<()>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| futures::lock::Mutex::new(()))
}

pub(crate) struct QueuePreview {
    image: Option<crate::attachments::CachedAttachmentImage>,
    finished: bool,
    // Keeping the task here cancels offscreen transfers on eviction.
    _task: gpui::Task<()>,
}

/// Providers whose in-turn steer carries staged images and typed selections
/// (mirrors the engine's admission gate).
fn steers_native_inputs(harness: luna_proto::HarnessId) -> bool {
    matches!(
        harness,
        luna_proto::HarnessId::Codex | luna_proto::HarnessId::ClaudeCode
    )
}

// Headless fixture: queued image steering per provider.
#[cfg(feature = "appshots-fixture")]
#[path = "queue_steer_e2e.rs"]
mod steer_e2e;
#[cfg(feature = "appshots-fixture")]
pub use steer_e2e::run_queue_image_steer_e2e;

fn visible_queue_rows(offset: f32, height: f32, count: usize) -> std::ops::Range<usize> {
    let first = ((-offset).max(0.0) / ROW_SLOT).floor() as usize;
    let last = first.saturating_add((height.max(0.0) / ROW_SLOT).ceil() as usize + 1);
    first.min(count)..last.min(count)
}

impl Composer {
    fn resolve_queued_primary(
        &self,
        chat_id: &str,
        item: &QueuedMessage,
        being_removed: bool,
        host_supports_actions: bool,
        cx: &gpui::App,
    ) -> QueuePrimaryResolution {
        let blocked = if being_removed {
            Some(QueuePrimaryBlock::Removing)
        } else if matches!(
            item.delivery_gate.as_ref(),
            Some(QueueDeliveryGate::Editing { .. })
        ) {
            Some(QueuePrimaryBlock::Editing)
        } else if matches!(
            item.delivery_gate.as_ref(),
            Some(QueueDeliveryGate::ReviewRequired { .. })
        ) {
            Some(QueuePrimaryBlock::ReviewRequired)
        } else {
            match &item.status {
                QueueMessageStatus::Held => None,
                QueueMessageStatus::Claimed { .. } => Some(QueuePrimaryBlock::Claimed),
                QueueMessageStatus::SteeringAccepted { .. } => {
                    Some(QueuePrimaryBlock::SteeringAccepted)
                }
                QueueMessageStatus::DeliveryUnknown { .. } => {
                    Some(QueuePrimaryBlock::DeliveryUnknown)
                }
            }
        }
        .or_else(|| {
            self.state
                .read(cx)
                .send_pending(chat_id, chrono::Utc::now())
                .then_some(QueuePrimaryBlock::DeliveryPending)
        })
        .or_else(|| (!host_supports_actions).then_some(QueuePrimaryBlock::HostUnsupported));

        let run_live = self.run_live(cx);
        let harness = item
            .execution
            .as_ref()
            .map(|execution| execution.harness)
            .or_else(|| self.pickers().read(cx).resolved(cx).harness);
        resolve_queue_primary_action(
            item.follow_up_behavior,
            run_live,
            self.pickers().read(cx).harness_steers_mid_turn(harness),
            !item.attachments.is_empty() && !harness.is_some_and(steers_native_inputs),
            item.hold_reason.is_some(),
            blocked,
        )
    }

    /// Cmd/Ctrl+Enter on an empty composer explicitly authorizes the newest
    /// queue row through the same resolver and L07 receipt path as its pointer
    /// action. Plain Enter remains an empty-composer no-op.
    pub(crate) fn activate_latest_queued_primary(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((chat_id, item, host_supports_actions)) = ({
            let state = self.state.read(cx);
            let queue = self.binding.queue(state);
            self.binding.chat_id(state).map(str::to_string).and_then(|chat_id| {
                queue
                    .iter()
                    .max_by_key(|item| item.issued_at)
                    .cloned()
                    .map(|item| {
                        let supported = state.chat_host_supports(
                            &chat_id,
                            luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1,
                        );
                        (chat_id, item, supported)
                    })
            })
        }) else {
            return false;
        };
        let resolution = self.resolve_queued_primary(
            &chat_id,
            &item,
            self.queue_removing.contains(&item.id),
            host_supports_actions,
            cx,
        );
        if let Some(blocked) = resolution.blocked {
            self.failure = Some(blocked.tooltip().into());
            cx.notify();
            return false;
        }
        self.activate_queued_primary(item.id, resolution.action, cx);
        true
    }

    /// The queue panel, or `None` when nothing is waiting. Like the composer,
    /// it is one frosted surface; rows use spacing and hover wash rather than
    /// nesting raised cards inside it.
    pub(crate) fn render_queue_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // A drop outside the panel ends GPUI's active drag without invoking our
        // `on_drop`. Never leave the source row replaced by a stale gap.
        if self.queue_drag.is_some() && !cx.has_active_drag() {
            self.queue_drag = None;
        }
        let (items, chat_id, host_supports_actions) = {
            let state = self.state.read(cx);
            let chat_id = self.binding.chat_id(state)?.to_string();
            let host_supports_actions = state
                .chat_host_supports(&chat_id, luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1);
            (self.binding.queue(state).to_vec(), chat_id, host_supports_actions)
        };
        self.prepare_queue_previews(&items, window, cx);
        if items.is_empty() {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let count = items.len();
        let drag = self
            .queue_drag
            .as_ref()
            .map(|d| (d.from, d.over, d.prev_over, d.epoch));
        let editing = self.editing_queued.clone();

        let list_chat = chat_id.clone();
        let drop_chat = chat_id.clone();
        let rows = queue_rows(
            &self.queue_scroll,
            window.viewport_size().height * 0.3,
            items.iter().enumerate().map(|(ix, item)| {
                self.queue_row(
                    &chat_id,
                    ix,
                    item,
                    drag,
                    &editing,
                    host_supports_actions,
                    &theme,
                    cx,
                )
            }),
        );

        let panel = queue_panel_surface(&theme)
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            // The complete glass surface is a drop target, including its
            // padding.
            .on_drag_move::<QueueDragPayload>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<QueueDragPayload>, _, cx| {
                    let payload = event.drag(cx);
                    if payload.chat != list_chat {
                        return;
                    }
                    let from = payload.from;
                    let rel_y = f32::from(event.event.position.y)
                        - f32::from(event.bounds.top())
                        - f32::from(this.queue_scroll.offset().y);
                    let over = queue_drop_index(rel_y, count);
                    this.update_queue_drag_over(from, over, cx);
                },
            ))
            .on_drop::<QueueDragPayload>(cx.listener(
                move |this, payload: &QueueDragPayload, _, cx| {
                    if payload.chat != drop_chat {
                        this.queue_drag = None;
                        cx.notify();
                        return;
                    }
                    let to = this
                        .queue_drag
                        .as_ref()
                        .map(|d| d.over)
                        .unwrap_or(payload.from);
                    this.queue_drag = None;
                    this.move_queued(payload.from, to, cx);
                },
            ))
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_queue_drag(cx)),
            )
            .child(rows);
        Some(crate::frost::frosted(PANEL_RADIUS, crate::frost::MENU_BLUR, panel).into_any_element())
    }

    /// One queued message: a quiet queue marker, the text, edit controls, and
    /// one explicit primary delivery action.
    #[allow(clippy::too_many_arguments)]
    fn queue_row(
        &self,
        chat_id: &str,
        ix: usize,
        item: &QueuedMessage,
        drag: Option<(usize, usize, usize, usize)>,
        editing: &Option<String>,
        host_supports_actions: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = SharedString::from(format!("queue-{}", item.id));
        let being_edited = editing.as_deref() == Some(item.id.as_str());
        let being_removed = self.queue_removing.contains(&item.id);
        let claim_blocked = !matches!(item.status, QueueMessageStatus::Held);
        let delivery_blocked = item.delivery_gate.is_some() || claim_blocked;
        let interaction_blocked = delivery_blocked || being_removed;
        let text = match (&item.status, &item.delivery_gate, item.hold_reason) {
            (
                QueueMessageStatus::Claimed {
                    mode: luna_doc::QueueClaimMode::SteerNow,
                    ..
                },
                _,
                _,
            ) if !being_edited => SharedString::from(format!(
                "Requesting steer… · {}",
                one_line(&queue_visible_text(&item.text, &item.attachments))
            )),
            (QueueMessageStatus::Claimed { .. }, _, _) if !being_edited => {
                SharedString::from(format!(
                    "Sending… · {}",
                    one_line(&queue_visible_text(&item.text, &item.attachments))
                ))
            }
            (QueueMessageStatus::SteeringAccepted { .. }, _, _) if !being_edited => {
                SharedString::from(format!(
                    "Steer accepted, pending · {}",
                    one_line(&queue_visible_text(&item.text, &item.attachments))
                ))
            }
            (
                QueueMessageStatus::DeliveryUnknown {
                    mode: luna_doc::QueueClaimMode::SteerNow,
                    ..
                },
                _,
                _,
            ) if !being_edited => SharedString::from(format!(
                "Steer outcome unknown · {}",
                one_line(&queue_visible_text(&item.text, &item.attachments))
            )),
            (QueueMessageStatus::DeliveryUnknown { .. }, _, _) if !being_edited => {
                SharedString::from(format!(
                    "Delivery unknown · {}",
                    one_line(&queue_visible_text(&item.text, &item.attachments))
                ))
            }
            (
                _,
                Some(QueueDeliveryGate::Editing {
                    owner_device_id, ..
                }),
                _,
            ) if !being_edited => SharedString::from(format!("Editing on {owner_device_id}")),
            (_, Some(QueueDeliveryGate::ReviewRequired { .. }), _) if !being_edited => {
                SharedString::from("Needs review")
            }
            (_, _, Some(reason)) if !being_edited => SharedString::from(format!(
                "{} · {}",
                queue_hold_reason_label(reason),
                one_line(&queue_visible_text(&item.text, &item.attachments))
            )),
            _ => one_line(&queue_visible_text(&item.text, &item.attachments)),
        };

        let edit_id = item.id.clone();
        let edit = self.queue_action(
            &key,
            "edit",
            "Edit",
            icons::PEN,
            !being_removed && !claim_blocked,
            theme,
            cx.listener(move |this, _, _, cx| {
                this.begin_queue_edit(edit_id.clone(), cx);
            }),
        );
        let drop_id = item.id.clone();
        let discard = self.queue_action(
            &key,
            "drop",
            if being_removed {
                "Removing…"
            } else {
                "Remove"
            },
            icons::TRASH_BIN_MINIMALISTIC,
            !being_removed && !claim_blocked,
            theme,
            cx.listener(move |this, _, _, cx| {
                this.remove_queued(drop_id.clone(), cx);
            }),
        );
        let resolved_primary =
            self.resolve_queued_primary(chat_id, item, being_removed, host_supports_actions, cx);
        let primary_action = resolved_primary.action;
        let primary_id = item.id.clone();
        let primary = self.queue_primary_action_button(
            &key,
            resolved_primary,
            theme,
            cx.listener(move |this, _, _, cx| {
                this.activate_queued_primary(primary_id.clone(), primary_action, cx);
            }),
        );
        let save = self.queue_action(
            &key,
            "save",
            "Save to queue",
            icons::QUEUE_CHECK,
            !self.queue_edit_finishing,
            theme,
            cx.listener(|this, _, _, cx| {
                this.commit_queue_edit(cx);
            }),
        );
        let cancel = self.queue_action(
            &key,
            "cancel",
            "Cancel",
            icons::QUEUE_CLOSE,
            !self.queue_edit_finishing,
            theme,
            cx.listener(|this, _, _, cx| {
                this.cancel_queue_edit(cx);
            }),
        );

        let drag_chat = chat_id.to_string();
        let queue_marker = div()
            .id(SharedString::from(format!("{key}-drag")))
            .w(px(14.0))
            .h(px(22.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .cursor_pointer()
            .when(interaction_blocked, |el| {
                el.cursor(gpui::CursorStyle::Arrow).opacity(0.35)
            })
            .child(
                icon(icons::QUEUE_DRAG_HANDLE)
                    .size(px(QUEUE_ICON_SIZE))
                    .text_color(theme.readable_muted(0.5)),
            );

        let row = div()
            .id(SharedString::from(format!("{key}-row")))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px(px(ROW_PAD_X))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(if self.queue_preview_limit() == 1 {
                4.0
            } else {
                8.0
            }))
            .rounded(px(ROW_RADIUS))
            .when(being_edited, |el| el.bg(crate::theme::ink(0.06)))
            .when(!being_edited && !being_removed, |el| {
                el.hover(|s| s.bg(crate::theme::ink(0.04)))
            })
            .when(being_removed, |el| el.opacity(0.55))
            .cursor(gpui::CursorStyle::Arrow)
            // The marker hints that the row belongs to the queue, while the
            // proven full-row drag hitbox keeps reordering easy. Editing
            // disables it so selection cannot become a reorder gesture.
            .when(!being_edited && !interaction_blocked, |el| {
                el.on_drag(
                    QueueDragPayload {
                        chat: drag_chat,
                        from: ix,
                    },
                    move |_payload, _point, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| QueueGhost)
                    },
                )
            })
            .when(!being_edited, |el| el.child(queue_marker))
            // Preserve text alignment while removing the disabled drag glyph
            // from the editing state.
            .when(being_edited, |el| el.child(div().w(px(14.0)).flex_none()))
            .when(!being_edited, |el| {
                let labels = queue_attachment_labels(&item.text, &item.attachments);
                let summary = if labels.len() > 1 {
                    format!("{} attachments · {}", labels.len(), labels.join(" · "))
                } else {
                    labels.join(" · ")
                };
                let only_images = text.as_ref() == crate::attachments::ATTACHMENT_ONLY_TEXT;
                let title = if only_images {
                    summary.clone().into()
                } else {
                    text
                };
                let mut content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(QUEUE_TEXT_SIZE))
                            .line_height(crate::typography::ui_rems(16.0))
                            .text_color(theme.text.opacity(0.9))
                            .child(title),
                    );
                if !labels.is_empty() && !only_images {
                    content = content.child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .line_height(crate::typography::ui_rems(13.0))
                            .text_color(theme.text_muted)
                            .child(summary),
                    );
                }
                el.children(
                    item.attachments
                        .iter()
                        .take(self.queue_preview_limit())
                        .enumerate()
                        .map(|(index, path)| self.queue_thumbnail(&key, index, path, cx)),
                )
                .when(item.attachments.len() > self.queue_preview_limit(), |el| {
                    let remaining = item.attachments.len() - self.queue_preview_limit();
                    el.child(
                        div()
                            .id(SharedString::from(format!("{key}-more-attachments")))
                            .w(px(28.0))
                            .h(px(28.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.0))
                            .bg(crate::theme::ink(0.06))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .aria_label(format!(
                                "{remaining} more attachments; edit message to view all"
                            ))
                            .child(format!("+{remaining}")),
                    )
                })
                .child(content)
            })
            .when(being_edited, |el| {
                el.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(QUEUE_TEXT_SIZE))
                        .text_color(theme.text_muted)
                        .child(if self.queue_edit_finishing {
                            "Saving…"
                        } else {
                            "Editing in composer"
                        }),
                )
            })
            .when(being_edited, |el| {
                el.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(3.0))
                        .child(save)
                        .child(cancel),
                )
            })
            .when(!being_edited, |el| {
                el.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(3.0))
                        .child(discard)
                        .child(edit)
                        .child(primary),
                )
            });

        let Some((from, over, prev_over, epoch)) = drag else {
            return row.into_any_element();
        };
        let (start, target) = queue_drag_offsets(ix, from, prev_over, over);
        if cx.reduce_motion() {
            return div()
                .relative()
                .top(px(target))
                .child(row)
                .into_any_element();
        }
        div()
            .child(row)
            .with_animation(
                ("queue-row-slide", (ix as u64) | ((epoch as u64) << 32)),
                TAB_SLIDE.animation(),
                move |el, t| el.relative().top(px(motion::lerp(start, target, t))),
            )
            .into_any_element()
    }

    pub(crate) fn release_queue_previews(&mut self, cx: &mut gpui::App) {
        for (_, preview) in self.queue_previews.drain() {
            if let Some(image) = preview.image {
                gpui::ImageSource::Image(image.image).evict(None, cx);
            }
        }
    }

    fn prepare_queue_previews(
        &mut self,
        items: &[QueuedMessage],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::attachments;
        let state = self.state.read(cx);
        let device = self
            .binding
            .chat_row(state)
            .map(|chat| chat.device_id.clone())
            .unwrap_or_default();
        let engine = state.engine().cloned();
        let target =
            (state.local_device_id.as_deref() != Some(device.as_str())).then(|| device.clone());
        let visible = visible_queue_rows(
            f32::from(self.queue_scroll.offset().y),
            f32::from(window.viewport_size().height) * 0.3,
            items.len(),
        );
        let keys: std::collections::HashSet<_> = items[visible]
            .iter()
            .flat_map(|item| item.attachments.iter().take(self.queue_preview_limit()))
            .map(|path| (device.clone(), path.clone()))
            .take(64)
            .collect();
        self.queue_previews.retain(|key, preview| {
            if keys.contains(key) {
                return true;
            }
            if let Some(image) = &preview.image {
                gpui::ImageSource::Image(image.image.clone()).evict(Some(window), cx);
            }
            false
        });
        let Some(engine) = engine else { return };
        for key in keys {
            if self.queue_previews.contains_key(&key) {
                continue;
            }
            let engine = engine.clone();
            let target = target.clone();
            let task_key = key.clone();
            let task = cx.spawn(async move |this, cx| {
                let _permit = preview_load_gate().lock().await;
                let source = match attachments::attachment_snapshot(&task_key.0, &task_key.1) {
                    attachments::AttachmentSnapshot::Loaded(image) => {
                        Some(attachments::LoadedAttachmentImage {
                            name: image.name.to_string(),
                            image: image.image,
                        })
                    }
                    _ => {
                        attachments::read_attachment_image(
                            &engine,
                            cx.background_executor(),
                            target.as_deref(),
                            &task_key.1,
                            None,
                        )
                        .await
                    }
                };
                let image = if let Some(source) = source {
                    cx.background_executor()
                        .spawn(async move {
                            attachments::queue_thumbnail_image(&source.image).map(|image| {
                                attachments::CachedAttachmentImage {
                                    name: source.name.into(),
                                    image,
                                }
                            })
                        })
                        .await
                } else {
                    None
                };
                this.update(cx, |this, cx| {
                    if let Some(preview) = this.queue_previews.get_mut(&task_key) {
                        preview.image = image;
                        preview.finished = true;
                    }
                    cx.notify();
                })
                .ok();
            });
            self.queue_previews.insert(
                key,
                QueuePreview {
                    image: None,
                    finished: false,
                    _task: task,
                },
            );
        }
    }

    fn load_queue_full_preview(&mut self, device: String, path: String, cx: &mut Context<Self>) {
        use crate::attachments;
        if let attachments::AttachmentSnapshot::Loaded(image) =
            attachments::attachment_snapshot(&device, &path)
        {
            self.queue_full_preview = None;
            self.show_queue_image(
                crate::attachments::PreviewImage::new(image.name, image.image),
                cx,
            );
            return;
        }
        let state = self.state.read(cx);
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        let target = (state.local_device_id.as_deref() != Some(device.as_str())).then_some(device);
        let chat = self.binding.chat_id(state).map(str::to_string);
        self.queue_full_preview = Some(cx.spawn(async move |this, cx| {
            let image = attachments::read_attachment_image(
                &engine,
                cx.background_executor(),
                target.as_deref(),
                &path,
                None,
            )
            .await;
            this.update(cx, |this, cx| {
                this.queue_full_preview = None;
                if this.binding.chat_id(this.state.read(cx)) != chat.as_deref() {
                    return;
                }
                if let Some(image) = image {
                    this.show_queue_image(
                        crate::attachments::PreviewImage::new(image.name, image.image),
                        cx,
                    );
                } else {
                    this.show_appshot_error(
                        "Could not load the image. Try opening it again.".into(),
                        cx,
                    );
                }
            })
            .ok();
        }));
    }

    fn queue_thumbnail(
        &self,
        key: &SharedString,
        index: usize,
        path: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::attachments;
        let device = self
            .binding
            .chat_row(self.state.read(cx))
            .map(|chat| chat.device_id.clone())
            .unwrap_or_default();
        let cache_key = (device.clone(), path.to_string());
        let failed = self
            .queue_previews
            .get(&cache_key)
            .is_some_and(|preview| preview.finished && preview.image.is_none());
        let snapshot = self
            .queue_previews
            .get(&cache_key)
            .and_then(|preview| preview.image.clone());
        let frame = div()
            .id(SharedString::from(format!("{key}-image-{index}")))
            .w(px(40.0))
            .h(px(28.0))
            .flex_none()
            .rounded(px(5.0))
            .border_1()
            .border_color(crate::theme::hairline(0.1))
            .bg(crate::theme::ink(0.035))
            .overflow_hidden();
        match snapshot {
            Some(image) => {
                let label = image.name.clone();
                let path = path.to_owned();
                let accent = Theme::of(cx).accent;
                frame
                    .role(gpui::Role::Button)
                    .aria_label(format!("Preview {}", label))
                    .tab_index(0)
                    .focus_visible(move |style| style.border_color(accent))
                    .hover(move |style| style.border_color(accent))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.load_queue_full_preview(device.clone(), path.clone(), cx);
                    }))
                    .child(
                        gpui::img(image.image)
                            .w(px(38.0))
                            .h(px(26.0))
                            .rounded(px(4.0))
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                    .into_any_element()
            }
            _ => frame
                .when(failed, |frame| {
                    let accent = Theme::of(cx).accent;
                    frame
                        .role(gpui::Role::Button)
                        .aria_label("Open attachment preview")
                        .tab_index(0)
                        .focus_visible(move |style| style.border_color(accent))
                        .cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.queue_previews.remove(&cache_key);
                            this.load_queue_full_preview(
                                cache_key.0.clone(),
                                cache_key.1.clone(),
                                cx,
                            );
                            cx.notify();
                        }))
                })
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icons::QUEUE_PAPERCLIP).size(px(14.0)))
                .into_any_element(),
        }
    }

    /// A permanently-visible trailing glyph button. The queue reference keeps
    /// edit and remove present instead of revealing them only on hover.
    fn queue_action(
        &self,
        key: &SharedString,
        slot: &str,
        label: &'static str,
        glyph: &'static str,
        enabled: bool,
        theme: &Theme,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        let own = SharedString::from(format!("{key}-{slot}-grp"));
        let accent = theme.accent;
        div()
            .id(SharedString::from(format!("{key}-{slot}")))
            .group(own.clone())
            .role(gpui::Role::Button)
            .aria_label(label)
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .opacity(0.72)
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.opacity(1.0).bg(crate::theme::ink(0.07)))
                    .tab_index(0)
                    .focus_visible(move |s| s.bg(accent.opacity(0.18)).text_color(accent))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        on_click(event, window, cx);
                    })
            })
            .when(!enabled, |el| {
                el.cursor(gpui::CursorStyle::Arrow).opacity(0.45)
            })
            .tooltip(move |_, cx| {
                cx.new(|_| QueueActionTooltip {
                    label: label.into(),
                })
                .into()
            })
            .tooltip_show_delay(std::time::Duration::from_millis(350))
            .child(
                icon(glyph)
                    .size(px(QUEUE_ICON_SIZE))
                    .text_color(theme.readable_muted(0.8))
                    .group_hover(own, |s| s.text_color(theme.text)),
            )
            .into_any_element()
    }

    /// Send now interrupts the current response before delivering the row.
    fn queue_primary_action_button(
        &self,
        key: &SharedString,
        resolution: QueuePrimaryResolution,
        theme: &Theme,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        let action = resolution.action;
        let enabled = resolution.enabled();
        let tooltip = resolution.tooltip();
        let accent = theme.accent;
        let compact = self.queue_preview_limit() == 1;
        div()
            .id(SharedString::from(format!("{key}-primary")))
            .role(gpui::Role::Button)
            .aria_label(tooltip)
            // Both labels occupy the same slot; modifier previews never move
            // the message text, thumbnails, or adjacent actions.
            .w(px(if compact { 28.0 } else { 72.0 }))
            .h(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted)
            .when(enabled, |el| {
                el.cursor_pointer()
                    .tab_index(0)
                    .hover(|s| s.bg(crate::theme::ink(0.07)).text_color(theme.text))
                    .focus_visible(move |s| s.bg(accent.opacity(0.18)).text_color(accent))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        on_click(event, window, cx);
                    })
            })
            .when(!enabled, |el| el.opacity(0.45))
            .tooltip(move |_, cx| {
                cx.new(|_| QueueActionTooltip {
                    label: tooltip.into(),
                })
                .into()
            })
            .tooltip_show_delay(std::time::Duration::from_millis(350))
            .child(if compact {
                icon(icons::QUEUE_SEND)
                    .size(px(QUEUE_ICON_SIZE))
                    .text_color(theme.text_muted)
                    .into_any_element()
            } else {
                div().child(action.label()).into_any_element()
            })
            .into_any_element()
    }

    /// Track the drop slot while a row is dragged over the list.
    fn update_queue_drag_over(&mut self, from: usize, over: usize, cx: &mut Context<Self>) {
        match &mut self.queue_drag {
            Some(drag) if drag.from == from => {
                if drag.over != over {
                    drag.prev_over = drag.over;
                    drag.over = over;
                    drag.epoch = drag.epoch.wrapping_add(1);
                    cx.notify();
                }
            }
            _ => {
                self.queue_drag = Some(QueueDragState {
                    from,
                    over,
                    prev_over: from,
                    epoch: 0,
                });
                cx.notify();
            }
        }
    }

    /// Restore a row whose pointer was released outside the queue's drop zone.
    fn cancel_queue_drag(&mut self, cx: &mut Context<Self>) {
        if self.queue_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Move the row at `from` to `to`, optimistically here and for real on the
    /// doc (the watch frame is what everyone else sees).
    pub(crate) fn move_queued(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to {
            cx.notify();
            return;
        }
        let Some(id) = self
            .binding
            .queue(self.state.read(cx))
            .get(from)
            .map(|item| item.id.clone())
        else {
            return;
        };
        let binding = self.binding.clone();
        self.state.update(cx, |state, cx| {
            if let Some(queue) = binding.queue_mut(state)
                && from < queue.len()
            {
                let item = queue.remove(from);
                queue.insert(to.min(queue.len()), item);
                cx.notify();
            }
        });
        self.queue_rpc(
            methods::MOVE_QUEUED_MESSAGE,
            serde_json::json!({ "id": id, "toIndex": to }),
            "Couldn't reorder the queue",
            cx,
        );
    }

    /// Cancel a queued message at its host. The row remains visible and inert
    /// until the host acknowledges winning the race against automatic drain.
    pub(crate) fn remove_queued(&mut self, id: String, cx: &mut Context<Self>) {
        if self.queue_removing.contains(&id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let (chat_id, host_device_id, revision, supported) = {
            let state = self.state.read(cx);
            let Some(chat_id) = self.binding.chat_id(state).map(str::to_string) else {
                return;
            };
            let Some(host_device_id) = self
                .binding
                .chat_row(state)
                .map(|chat| chat.device_id.clone())
            else {
                return;
            };
            let supported = state
                .chat_host_supports(&chat_id, luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1);
            let Some(revision) = self
                .binding
                .queue(state)
                .iter()
                .find(|item| item.id == id)
                .map(|item| item.revision)
            else {
                return;
            };
            (chat_id, host_device_id, revision, supported)
        };
        if !supported {
            self.failure = Some("The chat host does not support safe queue removal".into());
            cx.notify();
            return;
        }
        if self.editing_queued.as_deref() == Some(id.as_str()) {
            self.clear_queue_edit(cx);
        }
        self.queue_removing.insert(id.clone());
        self.queue_drag = None;
        cx.notify();

        let command_id = uuid::Uuid::new_v4().to_string();
        let intent = serde_json::json!({
            "kind": "removeQueuedMessage",
            "messageId": id,
            "expectedQueueRevision": revision,
        });
        cx.spawn(async move |this, cx| {
            let result = crate::task_commands::execute_receipt_on_with_id(
                &engine,
                &chat_id,
                Some(&host_device_id),
                command_id,
                intent,
            )
            .await;
            this.update(cx, |composer, cx| {
                composer.queue_removing.remove(&id);
                let selected_matches = composer.binding.chat_id(composer.state.read(cx))
                    == Some(chat_id.as_str());
                match result {
                    Ok(receipt) if receipt.status == luna_proto::CommandReceiptStatus::Applied => {
                        if selected_matches {
                            let binding = composer.binding.clone();
                            composer.state.update(cx, |state, cx| {
                                if let Some(queue) = binding.queue_mut(state) {
                                    queue.retain(|item| item.id != id);
                                }
                                cx.notify();
                            });
                        }
                    }
                    Ok(receipt) => {
                        tracing::debug!(
                            ?receipt,
                            "queued message had already left the queue before removal"
                        );
                        if selected_matches {
                            composer.failure = Some(
                                receipt
                                    .reason
                                    .map(|reason| reason.message)
                                    .unwrap_or_else(|| {
                                        "That message had already left the queue".into()
                                    })
                                    .into(),
                            );
                            let binding = composer.binding.clone();
                            composer
                                .state
                                .update(cx, |state, cx| binding.refresh_queue(state, cx));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "host-authoritative queue removal failed");
                        if selected_matches {
                            composer.failure = Some("Couldn't remove the message".into());
                            let binding = composer.binding.clone();
                            composer
                                .state
                                .update(cx, |state, cx| binding.refresh_queue(state, cx));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Send one now: the host stops the turn and hands this message over. Not
    /// optimistic — the row leaves the queue when the host has actually taken
    /// it, so a failed interrupt doesn't lose the text.
    pub(crate) fn send_queued_now(&mut self, id: String, cx: &mut Context<Self>) {
        if self.editing_queued.as_deref() == Some(id.as_str()) {
            self.clear_queue_edit(cx);
        }
        self.queue_rpc(
            methods::SEND_QUEUED_MESSAGE_NOW,
            serde_json::json!({ "id": id }),
            "Couldn't send that message",
            cx,
        );
    }

    pub(crate) fn steer_queued_now(&mut self, id: String, cx: &mut Context<Self>) {
        if self.editing_queued.as_deref() == Some(id.as_str()) {
            self.clear_queue_edit(cx);
        }
        self.queue_rpc(
            methods::STEER_QUEUED_MESSAGE_NOW,
            serde_json::json!({ "id": id }),
            "Couldn't steer that message",
            cx,
        );
    }

    /// Execute the same resolved action advertised on the row. Both pointer
    /// clicks and the empty-composer Enter gesture come through here.
    fn activate_queued_primary(
        &mut self,
        id: String,
        action: QueuePrimaryAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            QueuePrimaryAction::SendNow => self.send_queued_now(id, cx),
            QueuePrimaryAction::SteerNow => self.steer_queued_now(id, cx),
        }
    }

    /// Borrow the composer while the leased row reserves its queue position.
    pub(crate) fn begin_queue_edit(&mut self, id: String, cx: &mut Context<Self>) {
        if self.queue_edit_pending_id.is_some()
            || self.queue_edit_finishing
            || self.editing_queued.is_some()
            || !self.can_edit_queue_in_composer()
        {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let (chat_id, host_device_id, supported) = {
            let state = self.state.read(cx);
            let Some(chat_id) = self.binding.chat_id(state).map(str::to_string) else {
                return;
            };
            let Some(host_device_id) = self
                .binding
                .chat_row(state)
                .map(|chat| chat.device_id.clone())
            else {
                return;
            };
            let capability = luna_proto::capabilities::MESSAGE_QUEUE_EDIT_LEASE_V1;
            let supported = engine.engine_info().supports(capability)
                && state.chat_host_supports(&chat_id, capability);
            (chat_id, host_device_id, supported)
        };
        if !supported {
            self.failure = Some("Update the chat host to edit queued messages safely".into());
            cx.notify();
            return;
        }
        if !self.binding.queue(self.state.read(cx)).iter().any(|item| item.id == id) {
            return;
        }
        let owner_device_id = engine.engine_info().device_id.clone();
        let instance_id = self.queue_edit_instance_id.clone();
        self.queue_edit_pending_id = Some(id.clone());
        self.queue_drag = None;
        cx.notify();
        let params = serde_json::json!({
            "chatId": chat_id.clone(),
            "id": id,
            "editorDeviceId": owner_device_id,
            "editorInstanceId": instance_id,
            "targetDeviceId": host_device_id.clone(),
        });
        let task = cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::BEGIN_QUEUED_MESSAGE_EDIT, params)
                .await;
            let mut loaded_attachments = Vec::new();
            let mut loaded_appshots = Vec::new();
            if let Ok(reply) = &result
                && reply.get("outcome").and_then(|v| v.as_str()) == Some("acquired")
            {
                let paths = reply.get("attachments")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok());
                let mut load_failed = paths.is_none();
                for path in paths.unwrap_or_default() {
                    let loaded = crate::attachments::read_attachment_image(
                        &engine, cx.background_executor(), Some(&host_device_id), &path,
                        None,
                    ).await;
                    match loaded {
                        Some(loaded) => loaded_attachments.push(crate::attachments::StagedAttachment {
                            id: uuid::Uuid::new_v4().to_string(), name: loaded.name, image: loaded.image,
                        }),
                        None => { load_failed = true; break; }
                    }
                }
                if !load_failed {
                    let paths: Vec<String> = serde_json::from_value(reply["attachments"].clone()).unwrap_or_default();
                    match crate::appshots::restore_queued_appshots(
                        reply.get("text").and_then(|v| v.as_str()).unwrap_or_default(),
                        &paths, &loaded_attachments,
                    ) {
                        Ok((ordinary, shots)) => { loaded_attachments = ordinary; loaded_appshots = shots; }
                        Err(_) => { load_failed = true; }
                    }
                }
                if load_failed {
                    let _ = engine.client().call(methods::FINISH_QUEUED_MESSAGE_EDIT, serde_json::json!({
                        "chatId": chat_id, "id": id, "leaseId": reply.get("leaseId"),
                        "action": "cancel", "targetDeviceId": host_device_id,
                    })).await;
                    this.update(cx, |composer, cx| {
                        composer.queue_edit_pending_id = None;
                        composer.failure = Some("Couldn't load the queued attachments or Appshot context. Check the connection and update the chat host.".into());
                        cx.notify();
                    }).ok();
                    return;
                }
            }
            this.update(cx, |composer, cx| {
                composer.queue_edit_pending_id = None;
                match result {
                    Ok(reply)
                        if reply.get("outcome").and_then(|v| v.as_str()) == Some("acquired") =>
                    {
                        let Some(lease_id) = reply.get("leaseId").and_then(|v| v.as_str()) else {
                            composer.failure =
                                Some("The chat host returned an invalid edit lease".into());
                            cx.notify();
                            return;
                        };
                        let Some(base_text_hash) =
                            reply.get("baseTextHash").and_then(|v| v.as_str())
                        else {
                            composer.failure =
                                Some("The chat host returned an invalid edit lease".into());
                            cx.notify();
                            return;
                        };
                        let Some(revision) = reply.get("revision").and_then(|v| v.as_u64()) else {
                            composer.failure =
                                Some("The chat host returned an invalid queue revision".into());
                            cx.notify();
                            return;
                        };
                        let raw_text = reply
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let attachments: Vec<String> = serde_json::from_value(reply["attachments"].clone()).unwrap_or_default();
                        let text = queue_visible_text(&raw_text, &attachments);
                        let text = if !attachments.is_empty() && text == crate::attachments::ATTACHMENT_ONLY_TEXT {
                            String::new()
                        } else { text };
                        let selected_matches = composer.binding.chat_id(composer.state.read(cx))
                            == Some(chat_id.as_str());
                        if !selected_matches || !composer.can_edit_queue_in_composer() {
                            // Navigation or another composer action won acquisition. Release
                            // immediately; the expiry/review path is the backup.
                            let params = serde_json::json!({
                                "chatId": chat_id,
                                "id": id,
                                "leaseId": lease_id,
                                "action": "cancel",
                                "targetDeviceId": host_device_id,
                            });
                            let engine = engine.clone();
                            cx.spawn(async move |_, _| {
                                let _ = engine
                                    .client()
                                    .call(methods::FINISH_QUEUED_MESSAGE_EDIT, params)
                                    .await;
                            })
                            .detach();
                            return;
                        }
                        composer.editing_queued = Some(id.clone());
                        composer.queue_edit_lease_id = Some(lease_id.to_string());
                        composer.queue_edit_base_text_hash = Some(base_text_hash.to_string());
                        composer.queue_edit_revision = Some(revision);
                        composer.queue_edit_command_id = Some(uuid::Uuid::new_v4().to_string());
                        composer.queue_edit_chat_id = Some(chat_id.clone());
                        composer.queue_edit_host_device_id = Some(host_device_id.clone());
                        composer.schedule_draft_save(
                            composer.current_key.clone(),
                            std::time::Duration::ZERO,
                            cx,
                        );
                        composer.queue_edit_input_items = Some(composer.input_items.remove(&composer.current_key).unwrap_or_default());
                        let selected_inputs = serde_json::from_value(reply.get("inputItems").cloned().unwrap_or_else(|| serde_json::json!([]))).unwrap_or_default();
                        composer.input_items.insert(composer.current_key.clone(), selected_inputs);
                        composer.queue_edit_draft = Some((
                            composer.input.read(cx).text().to_string(),
                            composer.attachments.remove(&composer.current_key).unwrap_or_default(),
                            composer.appshots.remove(&composer.current_key).unwrap_or_default(),
                        ));
                        composer.attachments.insert(composer.current_key.clone(), loaded_attachments);
                        composer.appshots.insert(composer.current_key.clone(), loaded_appshots);
                        composer.focus_pending = true;
                        composer.set_input_text_without_draft_edit(text, cx);
                        let restored_key = composer.current_key.clone();
                        composer.trusted_native_envelope_restored(&restored_key, cx);
                        composer.start_queue_edit_renewal(engine.clone(), cx);
                    }
                    Ok(reply)
                        if reply.get("outcome").and_then(|v| v.as_str()) == Some("locked") =>
                    {
                        composer.failure =
                            Some("That queued message is being edited on another device".into());
                    }
                    Ok(reply)
                        if reply.get("outcome").and_then(|v| v.as_str()) == Some("claimed") =>
                    {
                        composer.failure = Some(
                            "That message has already been claimed for delivery".into(),
                        );
                    }
                    Ok(_) => {
                        composer.failure =
                            Some("That queued message is no longer available".into());
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "begin queue edit failed");
                        composer.failure =
                            Some("Connect to the chat host to edit this message".into());
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.queue_edit_task = Some(task);
    }

    /// Save the composer into the existing row, including its attachments.
    /// An entirely empty composer removes the row.
    pub(crate) fn commit_queue_edit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.editing_queued.is_none() {
            return false;
        }
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() && self.staged().is_empty() && self.staged_appshots().is_empty() {
            self.finish_queue_edit("discard", None, cx);
        } else {
            self.finish_queue_edit("commit", Some(text), cx);
        }
        true
    }

    /// Escape out of an edit, leaving the row as it was.
    pub(crate) fn cancel_queue_edit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.editing_queued.is_none() {
            return false;
        }
        self.finish_queue_edit("cancel", None, cx);
        true
    }

    pub(crate) fn clear_queue_edit(&mut self, cx: &mut Context<Self>) {
        self.release_queue_edit_best_effort(cx);
        self.clear_queue_edit_local(cx);
    }

    fn clear_queue_edit_local(&mut self, cx: &mut Context<Self>) {
        self.editing_queued = None;
        self.queue_edit_lease_id = None;
        self.queue_edit_base_text_hash = None;
        self.queue_edit_revision = None;
        self.queue_edit_command_id = None;
        self.queue_edit_chat_id = None;
        self.queue_edit_host_device_id = None;
        self.queue_edit_pending_id = None;
        self.queue_edit_finishing = false;
        self.input.update(cx, |input, cx| {
            input.read_only = false;
            cx.notify();
        });
        self.queue_edit_task = None;
        self.queue_edit_renew_task = None;
        if let Some((text, attachments, appshots)) = self.queue_edit_draft.take() {
            self.input_items.insert(
                self.current_key.clone(),
                self.queue_edit_input_items.take().unwrap_or_default(),
            );
            self.appshots.insert(self.current_key.clone(), appshots);
            self.set_input_text_without_draft_edit(text, cx);
            let restored_key = self.current_key.clone();
            self.trusted_native_envelope_restored(&restored_key, cx);
            self.attachments
                .insert(self.current_key.clone(), attachments);
        }
        self.focus_pending = true;
        cx.notify();
    }

    fn finish_queue_edit(
        &mut self,
        action: &'static str,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.queue_edit_finishing {
            return;
        }
        let (
            Some(id),
            Some(lease_id),
            Some(revision),
            Some(edit_command_id),
            Some(chat_id),
            Some(host_device_id),
            Some(engine),
        ) = (
            self.editing_queued.clone(),
            self.queue_edit_lease_id.clone(),
            self.queue_edit_revision,
            self.queue_edit_command_id.clone(),
            self.queue_edit_chat_id.clone(),
            self.queue_edit_host_device_id.clone(),
            self.state.read(cx).engine().cloned(),
        )
        else {
            self.failure = Some("The edit lease was lost; your text is still in the editor".into());
            cx.notify();
            return;
        };
        let expected = self.queue_edit_base_text_hash.clone();
        let mut staged = self.staged().to_vec();
        let input_items = self
            .input_items
            .get(&self.current_key)
            .cloned()
            .unwrap_or_default();
        let staged_appshots = self.staged_appshots().to_vec();
        staged.extend(staged_appshots.iter().map(|shot| shot.screenshot.clone()));
        let mut params = serde_json::json!({
            "chatId": chat_id.clone(),
            "id": id.clone(),
            "leaseId": lease_id.clone(),
            "action": action,
            "text": text,
            "expectedTextHash": expected.clone(),
            "targetDeviceId": host_device_id.clone(),
        });
        self.queue_edit_finishing = true;
        self.input.update(cx, |input, cx| {
            input.read_only = true;
            cx.notify();
        });
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let result = async {
                if action == "commit" {
                    let mut paths = Vec::new();
                    for attachment in &staged {
                        let upload_id = queue_edit_upload_id(&edit_command_id, &attachment.id);
                        let path = crate::attachments::upload_attachment(
                            &engine, cx.background_executor(), Some(&host_device_id),
                            &upload_id, attachment, None,
                        ).await.map_err(|err| format!("attachment upload: {err}"))?;
                        paths.push(path);
                    }
                    let appshot_paths = staged.iter().zip(&paths)
                        .map(|(attachment, path)| (attachment.id.clone(), path.clone())).collect();
                    params["text"] = crate::appshots::with_appshots(
                        params["text"].as_str().unwrap_or_default(), &staged_appshots, &appshot_paths,
                    ).into();
                    if params["text"].as_str().is_some_and(|text| text.trim().is_empty()) && !paths.is_empty() {
                        params["text"] = crate::attachments::ATTACHMENT_ONLY_TEXT.into();
                    }
                    params["attachments"] = serde_json::json!(paths);
                }
                if action == "commit" {
                    let intent = serde_json::json!({
                        "kind": "editQueuedMessage",
                        "messageId": id,
                        "expectedQueueRevision": revision,
                        "leaseId": lease_id,
                        "text": params["text"],
                        "expectedTextHash": params["expectedTextHash"],
                        "attachments": params["attachments"],
                        "inputItems": input_items,
                    });
                    let receipt = crate::task_commands::execute_receipt_on_with_id(
                        &engine,
                        &chat_id,
                        Some(&host_device_id),
                        edit_command_id,
                        intent,
                    )
                    .await
                    .map_err(|err| err.to_string())?;
                    let outcome = match receipt.status {
                        luna_proto::CommandReceiptStatus::Applied => "committed",
                        luna_proto::CommandReceiptStatus::Conflict => match receipt
                            .reason
                            .as_ref()
                            .map(|reason| reason.code)
                        {
                            Some(luna_proto::CommandReasonCode::QueueClaimed) => "claimed",
                            Some(luna_proto::CommandReasonCode::QueueMissing) => "missing",
                            _ => "conflict",
                        },
                        _ => "lost",
                    };
                    Ok(serde_json::json!({
                        "outcome": outcome,
                        "message": receipt.reason.map(|reason| reason.message),
                    }))
                } else {
                    crate::attachments::call_with_timeout(
                        &engine, cx.background_executor(), methods::FINISH_QUEUED_MESSAGE_EDIT,
                        params, std::time::Duration::from_secs(30),
                    ).await.map_err(|err| err.to_string())
                }
            }.await;
            this.update(cx, |composer, cx| {
                composer.queue_edit_finishing = false;
                composer.input.update(cx, |input, cx| { input.read_only = false; cx.notify(); });
                match result {
                    Ok(reply) => match reply.get("outcome").and_then(|v| v.as_str()) {
                        Some("committed" | "cancelled" | "discarded" | "released") => {
                            composer.clear_queue_edit_local(cx);
                            return;
                        }
                        Some("conflict") => {
                            composer.failure = Some(
                                "This message changed on another device; your edit was kept locally".into(),
                            );
                        }
                        Some("missing") => {
                            composer.failure = Some(
                                "The queued message was removed; your edit was kept locally".into(),
                            );
                        }
                        Some("claimed") => {
                            composer.failure = Some(
                                "This message was claimed for delivery; your edit was kept locally"
                                    .into(),
                            );
                        }
                        _ => {
                            composer.failure = Some(
                                "The edit lease changed; your text is still in the editor".into(),
                            );
                        }
                    },
                    Err(err) => {
                        tracing::warn!(error = %err, "finish queue edit failed");
                        composer.failure = Some(if err.starts_with("attachment upload:") {
                            "Couldn't upload the attachment; your edit is still in the editor"
                                .into()
                        } else {
                            "Couldn't save the queue edit; your edit is still in the editor".into()
                        });
                    }
                }
                cx.notify();
            }).ok();
        });
        self.queue_edit_task = Some(task);
    }

    fn start_queue_edit_renewal(
        &mut self,
        engine: crate::state::EngineHandle,
        cx: &mut Context<Self>,
    ) {
        let (Some(id), Some(lease_id), Some(chat_id), Some(host_device_id)) = (
            self.editing_queued.clone(),
            self.queue_edit_lease_id.clone(),
            self.queue_edit_chat_id.clone(),
            self.queue_edit_host_device_id.clone(),
        ) else {
            return;
        };
        self.queue_edit_renew_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(20))
                    .await;
                let params = serde_json::json!({
                    "chatId": chat_id,
                    "id": id,
                    "leaseId": lease_id,
                    "targetDeviceId": host_device_id,
                });
                match engine
                    .client()
                    .call(methods::RENEW_QUEUED_MESSAGE_EDIT, params)
                    .await
                {
                    Ok(reply)
                        if reply.get("outcome").and_then(|v| v.as_str()) == Some("renewed") => {}
                    Ok(_) => {
                        this.update(cx, |composer, cx| {
                            composer.failure = Some(
                                "Edit protection expired; review this message before sending"
                                    .into(),
                            );
                            cx.notify();
                        })
                        .ok();
                        break;
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "queue edit heartbeat failed");
                        // A transient miss is tolerated by the 60s lease. Keep
                        // trying; the host fails closed if all attempts miss.
                    }
                }
            }
        }));
    }

    fn release_queue_edit_best_effort(&self, cx: &mut Context<Self>) {
        let (Some(id), Some(lease_id), Some(chat_id), Some(host_device_id), Some(engine)) = (
            self.editing_queued.clone(),
            self.queue_edit_lease_id.clone(),
            self.queue_edit_chat_id.clone(),
            self.queue_edit_host_device_id.clone(),
            self.state.read(cx).engine().cloned(),
        ) else {
            return;
        };
        let params = serde_json::json!({
            "chatId": chat_id,
            "id": id,
            "leaseId": lease_id,
            "action": "cancel",
            "targetDeviceId": host_device_id,
        });
        cx.spawn(async move |_, _| {
            let _ = engine
                .client()
                .call(methods::FINISH_QUEUED_MESSAGE_EDIT, params)
                .await;
        })
        .detach();
    }

    /// Fire one queue mutation at the chat's doc host.
    fn queue_rpc(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        failure: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let (chat_id, host_device_id, host_supports_action) = {
            let state = self.state.read(cx);
            let Some(chat_id) = self.binding.chat_id(state).map(str::to_string) else {
                return;
            };
            let host = queue_action_needs_host(method)
                .then(|| {
                    self.binding
                        .chat_row(state)
                        .map(|chat| chat.device_id.clone())
                })
                .flatten();
            let supported = !queue_action_needs_host(method)
                || state.chat_host_supports(
                    &chat_id,
                    luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1,
                );
            (chat_id, host, supported)
        };
        if !host_supports_action {
            self.failure = Some("The chat host does not support queue actions".into());
            cx.notify();
            return;
        }
        let pending_message = matches!(
            method,
            methods::SEND_QUEUED_MESSAGE_NOW | methods::STEER_QUEUED_MESSAGE_NOW
        )
        .then(|| {
            params
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
        })
        .flatten();
        if let Some(id) = &pending_message {
            self.state.update(cx, |state, cx| {
                state.begin_pending_send(&chat_id, id, chrono::Utc::now());
                cx.notify();
            });
        }
        let queue_revision = pending_message.as_ref().and_then(|id| {
            self.binding
                .queue(self.state.read(cx))
                .iter()
                .find(|item| item.id == *id)
                .map(|item| item.revision)
        });
        let task_intent = pending_message.as_ref().and_then(|id| match method {
            methods::SEND_QUEUED_MESSAGE_NOW => Some(serde_json::json!({
                "kind": "sendQueuedNow",
                "messageId": id,
                "expectedQueueRevision": queue_revision,
            })),
            methods::STEER_QUEUED_MESSAGE_NOW => Some(serde_json::json!({
                "kind": "steerQueuedNow",
                "messageId": id,
                "expectedQueueRevision": queue_revision,
            })),
            _ => None,
        });
        let command_id = uuid::Uuid::new_v4().to_string();
        let task_target = host_device_id.clone();
        let mut params = params;
        if let Some(object) = params.as_object_mut() {
            object.insert("chatId".into(), serde_json::Value::String(chat_id.clone()));
            if let Some(host) = host_device_id {
                object.insert("targetDeviceId".into(), serde_json::Value::String(host));
            }
        }
        // Detached, not held: these are independent one-shot mutations, and
        // parking them in a single slot meant the next arrow tap dropped — and
        // so cancelled — the move still in flight, leaving the optimistic list
        // showing an order the doc never got.
        cx.spawn(async move |this, cx| {
            match if let Some(intent) = task_intent {
                crate::task_commands::execute_receipt_on_with_id(
                    &engine,
                    &chat_id,
                    task_target.as_deref(),
                    command_id,
                    intent,
                )
                .await
                .map(|receipt| {
                    serde_json::json!({
                        "sent": matches!(
                            receipt.status,
                            luna_proto::CommandReceiptStatus::Pending
                                | luna_proto::CommandReceiptStatus::Applied
                        ),
                        "receipt": receipt,
                    })
                })
            } else {
                engine.client().call(method, params).await
            } {
                Ok(reply) if queue_mutation_acknowledged(method, &reply) => {
                    // The host has adopted this message. Clear even if the
                    // user switched chats and its transcript is no longer watched.
                    if let Some(id) = &pending_message {
                        this.update(cx, |composer, cx| {
                            composer.state.update(cx, |state, cx| {
                                state.end_pending_send(&chat_id, id);
                                cx.notify();
                            });
                        })
                        .ok();
                    }
                }
                Ok(reply) => {
                    tracing::debug!(
                        method,
                        ?reply,
                        "queue mutation was not applied; reconciling"
                    );
                    this.update(cx, |composer, cx| {
                        if let Some(id) = &pending_message {
                            composer.state.update(cx, |state, cx| {
                                state.end_pending_send(&chat_id, id);
                                cx.notify();
                            });
                        }
                        let binding = composer.binding.clone();
                        composer
                            .state
                            .update(cx, |state, cx| binding.refresh_queue(state, cx));
                    })
                    .ok();
                }
                Err(err) => {
                    tracing::warn!(method, error = %err, "queue mutation failed");
                    this.update(cx, |composer, cx| {
                        if let Some(id) = &pending_message {
                            composer.state.update(cx, |state, cx| {
                                state.end_pending_send(&chat_id, id);
                                cx.notify();
                            });
                        }
                        composer.failure = Some(failure.into());
                        let binding = composer.binding.clone();
                        composer
                            .state
                            .update(cx, |state, cx| binding.refresh_queue(state, cx));
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;
    use gpui::AppContext as _;
    use luna_doc::{QueueClaimMode, QueueHoldReason, QueueMessageStatus, QueuedMessage};
    use luna_proto::{
        Chat, CommandReceipt, CommandReceiptStatus, Device, FollowUpBehavior, TaskCommandRead,
        TaskCommandSnapshot,
    };
    use luna_rpc::{RpcError, RpcReply, RpcService, methods};

    use super::{
        PANEL_PAD_TOP, QueuePrimaryAction, QueuePrimaryBlock, QueuePrimaryResolution, ROW_SLOT,
        one_line, queue_action_needs_host, queue_drag_offsets, queue_drop_index,
        queue_edit_upload_id, queue_mutation_acknowledged, queue_visible_text,
        resolve_queue_primary_action, visible_queue_rows,
    };
    use crate::composer::Composer;
    use crate::state::{AppState, EngineHandle};

    #[derive(Default)]
    struct QueueActionRpc {
        executes: Mutex<Vec<serde_json::Value>>,
    }

    #[async_trait]
    impl RpcService for QueueActionRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                methods::GET_TASK_COMMAND_SNAPSHOT => RpcReply::value(&TaskCommandRead {
                    snapshot: TaskCommandSnapshot {
                        task_id: "chat".into(),
                        task_revision: 3,
                        task_sequence: 4,
                        binding_id: Some("binding".into()),
                        binding_generation: Some(5),
                        active_turn_id: None,
                    },
                    events: Vec::new(),
                    reload_required: false,
                    next_task_sequence: None,
                    has_more: false,
                }),
                methods::EXECUTE_TASK_COMMAND => {
                    self.executes.lock().unwrap().push(params.clone());
                    RpcReply::value(&CommandReceipt {
                        command_id: params["commandId"].as_str().unwrap().into(),
                        task_id: "chat".into(),
                        intent: "sendQueuedNow".into(),
                        payload_hash: "fixture".into(),
                        status: CommandReceiptStatus::Applied,
                        snapshot: TaskCommandSnapshot {
                            task_id: "chat".into(),
                            task_revision: 3,
                            task_sequence: 4,
                            binding_id: Some("binding".into()),
                            binding_generation: Some(5),
                            active_turn_id: None,
                        },
                        event_sequence: Some(5),
                        actor_id: "local".into(),
                        client_id: "local".into(),
                        client_session_id: "fixture".into(),
                        execution_environment: Some("host".into()),
                        correlation_id: Some("held-steer".into()),
                        reason: None,
                        recovery_action: None,
                        accepted_at_ms: 1,
                        committed_at_ms: Some(2),
                        observed_at_ms: 2,
                    })
                }
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    fn test_chat() -> Chat {
        Chat {
            child_thread: None,
            id: "chat".into(),
            device_id: "host".into(),
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
            last_message_at: None,
            created_at: Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: None,
        }
    }

    fn held_steer(status: QueueMessageStatus) -> QueuedMessage {
        QueuedMessage {
            input_items: Vec::new(),
            id: "held-steer".into(),
            text: "explicitly authorize me".into(),
            attachments: Vec::new(),
            hold_for_turn_end: true,
            follow_up_behavior: FollowUpBehavior::Steer,
            hold_reason: Some(QueueHoldReason::ProviderRejected),
            issued_by: "local".into(),
            issued_at: 7,
            edited_at: None,
            revision: 9,
            execution: None,
            status,
            delivery_gate: None,
        }
    }

    #[test]
    fn queue_edit_upload_ids_are_stable_distinct_and_accepted_by_the_upload_jail() {
        let command = "6b58f3ff-a0d8-4bf7-96b6-3f501f4d7cec";
        let first = queue_edit_upload_id(command, "e9337085-cdad-40f7-9132-2286a21d6071");
        let retry = queue_edit_upload_id(command, "e9337085-cdad-40f7-9132-2286a21d6071");
        let second = queue_edit_upload_id(command, "ca5ee696-81b7-4791-b59f-2de48beffe36");
        assert_eq!(first, retry);
        assert_ne!(first, second);
        assert!(first.len() <= 64);
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );

        let root = tempfile::tempdir().unwrap();
        let uploads = luna_engine::Uploads::new(root.path());
        uploads.append(&first, "QQ==", Some(0)).unwrap();
        let committed = uploads.commit(&first, "queue-edit.png").unwrap();
        assert!(std::path::Path::new(&committed).is_file());
    }

    #[test]
    fn queue_preview_work_follows_the_visible_rows() {
        assert_eq!(visible_queue_rows(0.0, ROW_SLOT * 3.0, 1000), 0..4);
        assert_eq!(
            visible_queue_rows(-ROW_SLOT * 20.0, ROW_SLOT * 3.0, 1000),
            20..24
        );
        assert_eq!(
            visible_queue_rows(-ROW_SLOT * 20.0, ROW_SLOT * 3.0, 0),
            0..0
        );
    }

    #[test]
    fn primary_action_distinguishes_explicit_override_from_delivery_fences() {
        for (behavior, live, steerable, attachments, held, action, case) in [
            (
                FollowUpBehavior::Queue,
                false,
                true,
                false,
                false,
                QueuePrimaryAction::SendNow,
                "ordinary Queue",
            ),
            (
                FollowUpBehavior::Steer,
                true,
                true,
                false,
                false,
                QueuePrimaryAction::SteerNow,
                "live safe Steer",
            ),
            (
                FollowUpBehavior::Steer,
                false,
                false,
                false,
                true,
                QueuePrimaryAction::SendNow,
                "idle rejected Steer",
            ),
            (
                FollowUpBehavior::Steer,
                true,
                true,
                true,
                true,
                QueuePrimaryAction::SendNow,
                "attachment Steer",
            ),
            (
                FollowUpBehavior::Steer,
                true,
                true,
                false,
                true,
                QueuePrimaryAction::SendNow,
                "live rejected Steer",
            ),
        ] {
            assert_eq!(
                resolve_queue_primary_action(behavior, live, steerable, attachments, held, None,),
                QueuePrimaryResolution {
                    action,
                    blocked: None,
                },
                "{case}"
            );
        }
        assert_eq!(
            resolve_queue_primary_action(
                luna_proto::FollowUpBehavior::Steer,
                false,
                false,
                false,
                true,
                Some(QueuePrimaryBlock::DeliveryUnknown),
            ),
            QueuePrimaryResolution {
                action: QueuePrimaryAction::SendNow,
                blocked: Some(QueuePrimaryBlock::DeliveryUnknown),
            },
            "unknown delivery remains fenced despite the Send now presentation"
        );
        for (block, tooltip) in [
            (
                QueuePrimaryBlock::HostUnsupported,
                "The chat host does not support queue actions",
            ),
            (
                QueuePrimaryBlock::Removing,
                "Removal is already in progress",
            ),
            (QueuePrimaryBlock::Editing, "Finish editing before sending"),
            (
                QueuePrimaryBlock::ReviewRequired,
                "Review this message before sending",
            ),
            (
                QueuePrimaryBlock::DeliveryPending,
                "A delivery is already in progress",
            ),
            (
                QueuePrimaryBlock::Claimed,
                "This message is already being delivered",
            ),
            (
                QueuePrimaryBlock::SteeringAccepted,
                "Accepted and pending native recording; model receipt is unconfirmed",
            ),
            (
                QueuePrimaryBlock::DeliveryUnknown,
                "Delivery outcome is unknown",
            ),
        ] {
            assert_eq!(block.tooltip(), tooltip);
        }
    }

    #[gpui::test]
    fn empty_modifier_queue_action_executes_one_fenced_send_now_and_blocks_unknown(
        cx: &mut gpui::TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let rpc = Arc::new(QueueActionRpc::default());
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.set_test_engine(EngineHandle::from_test_client(luna_rpc::memory_client(
                rpc.clone(),
            )));
            state.chats = vec![test_chat()];
            state.devices = vec![Device {
                id: "host".into(),
                name: "Synthetic host".into(),
                platform: "macos".into(),
                last_seen_at: None,
                created_at: None,
                version: None,
                capabilities: vec![luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1.into()],
            }];
            state.selected_chat = Some("chat".into());
            state.queue = vec![held_steer(QueueMessageStatus::Held)];
            state
        });
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));

        composer.update(cx, |composer, cx| {
            assert!(composer.activate_latest_queued_primary(cx));
        });
        for _ in 0..100 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            if rpc.executes.lock().unwrap().len() == 1 {
                break;
            }
        }
        let calls = rpc.executes.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "the shortcut must create one receipt command"
        );
        assert_eq!(calls[0]["intent"]["kind"], "sendQueuedNow");
        assert_eq!(calls[0]["intent"]["messageId"], "held-steer");
        assert_eq!(calls[0]["intent"]["expectedQueueRevision"], 9);
        assert_eq!(calls[0]["expectedBindingGeneration"], 5);
        assert_eq!(calls[0]["targetDeviceId"], "host");
        drop(calls);
        runtime.block_on(tokio::task::yield_now());
        cx.run_until_parked();

        state.update(cx, |state, _| {
            state.queue = vec![held_steer(QueueMessageStatus::DeliveryUnknown {
                command_id: "unknown".into(),
                mode: QueueClaimMode::SteerNow,
                since_ms: 10,
            })];
        });
        composer.update(cx, |composer, cx| {
            assert!(!composer.activate_latest_queued_primary(cx));
            assert_eq!(
                composer.failure.as_deref(),
                Some("Delivery outcome is unknown")
            );
        });
        cx.run_until_parked();
        assert_eq!(
            rpc.executes.lock().unwrap().len(),
            1,
            "unknown delivery must not create another command"
        );
    }

    #[test]
    fn host_authoritative_queue_actions_route_to_the_host() {
        assert!(queue_action_needs_host(methods::SEND_QUEUED_MESSAGE_NOW));
        assert!(queue_action_needs_host(methods::STEER_QUEUED_MESSAGE_NOW));
        assert!(queue_action_needs_host(methods::REMOVE_QUEUED_MESSAGE));
        assert!(queue_action_needs_host(methods::BEGIN_QUEUED_MESSAGE_EDIT));
        assert!(queue_action_needs_host(methods::RENEW_QUEUED_MESSAGE_EDIT));
        assert!(queue_action_needs_host(methods::FINISH_QUEUED_MESSAGE_EDIT));
        assert!(!queue_action_needs_host(methods::QUEUE_MESSAGE));
        assert!(queue_action_needs_host(methods::UPDATE_QUEUED_MESSAGE));
        assert!(queue_action_needs_host(methods::MOVE_QUEUED_MESSAGE));
    }

    /// Exercise the real composer -> memory RPC -> queue subscription flow.
    /// Both false ACKs and owner/transport errors must undo optimistic moves
    /// even when the underlying document emits no change notification.
    #[gpui::test]
    fn rejected_owner_queue_move_restores_the_watched_order(cx: &mut gpui::TestAppContext) {
        struct RejectedMoveRpc {
            items: Vec<QueuedMessage>,
            deny: bool,
            calls: Mutex<Vec<(String, serde_json::Value)>>,
        }
        #[async_trait]
        impl RpcService for RejectedMoveRpc {
            async fn handle(
                &self,
                method: &str,
                params: serde_json::Value,
            ) -> Result<RpcReply, RpcError> {
                self.calls.lock().unwrap().push((method.into(), params));
                match method {
                    methods::MOVE_QUEUED_MESSAGE if self.deny => {
                        Err(RpcError::Failed("synthetic owner denial".into()))
                    }
                    methods::MOVE_QUEUED_MESSAGE => {
                        RpcReply::value(&serde_json::json!({"changed":false}))
                    }
                    methods::WATCH_QUEUE => {
                        use futures::StreamExt as _;
                        let frame = serde_json::json!({"items":self.items});
                        Ok(RpcReply::Stream(Box::pin(
                            futures::stream::once(async move { frame })
                                .chain(futures::stream::pending()),
                        )))
                    }
                    other => Err(RpcError::UnknownMethod(other.into())),
                }
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        for deny in [false, true] {
            let first = held_steer(QueueMessageStatus::Held);
            let mut second = first.clone();
            second.id = "second-row".into();
            let original = vec![first, second];
            let rpc = Arc::new(RejectedMoveRpc {
                items: original.clone(),
                deny,
                calls: Mutex::default(),
            });
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(EngineHandle::from_authenticated_remote(
                    luna_rpc::memory_client(rpc.clone()),
                    luna_proto::EngineInfo {
                        device_id: "local".into(),
                        workspace_scope: luna_proto::WorkspaceScope::Local,
                        capabilities: vec![luna_proto::capabilities::MESSAGE_QUEUE_V1.into()],
                    },
                    "memory://owner-rollback".into(),
                ));
                state.chats = vec![test_chat()];
                state.devices = vec![Device {
                    id: "host".into(),
                    name: "Synthetic host".into(),
                    platform: "macos".into(),
                    last_seen_at: None,
                    created_at: None,
                    version: None,
                    capabilities: vec![luna_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1.into()],
                }];
                state.selected_chat = Some("chat".into());
                state.queue = original.clone();
                state
            });
            let composer = cx.new(|cx| Composer::new(state.clone(), cx));
            composer.update(cx, |composer, cx| composer.move_queued(0, 1, cx));
            assert_eq!(
                state.read_with(cx, |state, _| state.queue[0].id.clone()),
                "second-row",
                "move starts optimistically"
            );
            for _ in 0..100 {
                cx.run_until_parked();
                runtime.block_on(tokio::task::yield_now());
                if state.read_with(cx, |state, _| state.queue == original) {
                    break;
                }
            }
            assert_eq!(
                state.read_with(cx, |state, _| state.queue.clone()),
                original,
                "rejected move must roll back; denial={deny}"
            );
            let calls = rpc.calls.lock().unwrap();
            let moves = calls
                .iter()
                .filter(|(method, _)| method == methods::MOVE_QUEUED_MESSAGE)
                .collect::<Vec<_>>();
            assert_eq!(moves.len(), 1, "rejection must not replay the mutation");
            assert_eq!(moves[0].1["targetDeviceId"], "host");
            assert!(
                calls
                    .iter()
                    .any(|(method, _)| method == methods::WATCH_QUEUE)
            );
        }
    }

    #[test]
    fn mutation_acknowledgements_detect_conflicts_and_malformed_replies() {
        assert!(queue_mutation_acknowledged(
            methods::MOVE_QUEUED_MESSAGE,
            &serde_json::json!({ "changed": true })
        ));
        assert!(!queue_mutation_acknowledged(
            methods::MOVE_QUEUED_MESSAGE,
            &serde_json::json!({ "changed": false })
        ));
        assert!(!queue_mutation_acknowledged(
            methods::REMOVE_QUEUED_MESSAGE,
            &serde_json::json!({})
        ));
        assert!(queue_mutation_acknowledged(
            methods::SEND_QUEUED_MESSAGE_NOW,
            &serde_json::json!({ "sent": true })
        ));
    }

    #[test]
    fn the_whole_panel_maps_to_a_clamped_queue_drop_slot() {
        assert_eq!(queue_drop_index(0.0, 2), 0, "top pad targets the head");
        assert_eq!(queue_drop_index(PANEL_PAD_TOP + ROW_SLOT - 0.1, 2), 0);
        assert_eq!(queue_drop_index(PANEL_PAD_TOP + ROW_SLOT, 2), 1);
        assert_eq!(queue_drop_index(10_000.0, 2), 1);
    }

    #[test]
    fn drag_offsets_move_the_real_row_and_open_its_destination() {
        assert_eq!(queue_drag_offsets(0, 0, 0, 2), (0.0, 2.0 * ROW_SLOT));
        assert_eq!(queue_drag_offsets(1, 0, 0, 2), (0.0, -ROW_SLOT));
        assert_eq!(queue_drag_offsets(2, 0, 0, 2), (0.0, -ROW_SLOT));

        // Moving the pointer back one slot restarts only the rows whose
        // visual destination actually changed.
        assert_eq!(queue_drag_offsets(0, 0, 2, 1), (2.0 * ROW_SLOT, ROW_SLOT));
        assert_eq!(queue_drag_offsets(1, 0, 2, 1), (-ROW_SLOT, -ROW_SLOT));
        assert_eq!(queue_drag_offsets(2, 0, 2, 1), (-ROW_SLOT, 0.0));
    }

    /// A row is one line tall, so a multi-line message has to read as one line
    /// — otherwise the panel's rows stop lining up.
    #[test]
    fn rows_flatten_multi_line_messages() {
        assert_eq!(
            one_line("fix the test\n\nthen ship it").as_ref(),
            "fix the test then ship it"
        );
        assert_eq!(one_line("  spaced   out  ").as_ref(), "spaced out");
    }

    #[test]
    fn appshot_context_is_hidden_in_clean_and_legacy_queue_rows() {
        let shot = crate::appshots::tests::shot();
        let paths: Vec<String> = vec!["/host/image.png".into()];
        for user_text in ["inspect this", ""] {
            let body = crate::appshots::with_appshots(
                user_text,
                &[shot.clone()],
                &std::collections::HashMap::from([(shot.screenshot.id.clone(), paths[0].clone())]),
            );
            let expected = if user_text.is_empty() {
                crate::attachments::ATTACHMENT_ONLY_TEXT
            } else {
                user_text
            };
            assert_eq!(queue_visible_text(&body, &paths), expected);
            assert_eq!(
                queue_visible_text(&crate::attachments::with_attachments(&body, &paths), &paths),
                expected
            );
        }
    }

    #[test]
    fn attachment_labels_decode_app_names_and_preserve_ordinary_images() {
        let shot = crate::appshots::tests::shot();
        let paths = vec![
            "/tmp/shot & detail.png".to_owned(),
            "/tmp/reference.png".to_owned(),
        ];
        let mut shot = shot;
        shot.app_name = "Notes & Ideas".into();
        let body = crate::appshots::with_appshots(
            "look",
            &[shot.clone()],
            &[(shot.screenshot.id.clone(), paths[0].clone())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            super::queue_attachment_labels(&body, &paths),
            vec!["Notes & Ideas Appshot", "reference.png"]
        );
        assert_eq!(
            super::queue_attachment_labels(&body, &["/tmp/other.png".into()]),
            vec!["other.png"]
        );
        let malformed = format!("\n\n{}\n<appshot", crate::appshots::CONTEXT_MARKER);
        assert_eq!(
            super::queue_attachment_labels(&malformed, &paths),
            vec!["shot & detail.png", "reference.png"]
        );
    }

    #[test]
    fn legacy_attachment_trailers_are_hidden_from_queue_text() {
        let paths = vec!["/tmp/image.png".to_string()];
        let legacy = crate::attachments::with_attachments("inspect this", &paths);
        assert_eq!(queue_visible_text(&legacy, &paths), "inspect this");

        let image_only = crate::attachments::with_attachments("", &paths);
        assert_eq!(
            queue_visible_text(&image_only, &paths),
            crate::attachments::ATTACHMENT_ONLY_TEXT
        );
        assert_eq!(
            queue_visible_text("literal user text", &paths),
            "literal user text"
        );
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;
    use gpui::{AppContext, ScrollHandle, TestAppContext, point};

    struct QueueScrollTestView {
        queue: ScrollHandle,
        transcript: ScrollHandle,
        count: usize,
    }

    impl Render for QueueScrollTestView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(
                    div()
                        .id("transcript-underlay")
                        .absolute()
                        .inset_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.transcript)
                        .child(div().h(px(2000.0))),
                )
                .child(
                    div().absolute().bottom_0().w_full().child(
                        queue_panel_surface(Theme::of(cx)).child(queue_rows(
                            &self.queue,
                            px(180.0),
                            (0..self.count)
                                .map(|_| div().h(px(ROW_HEIGHT)).flex_none().into_any_element()),
                        )),
                    ),
                )
        }
    }

    #[gpui::test]
    fn queue_wheel_does_not_scroll_the_transcript_even_at_its_boundaries(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (view, cx) = cx.add_window_view(|_, _| QueueScrollTestView {
            queue: ScrollHandle::new(),
            transcript: ScrollHandle::new(),
            count: 24,
        });
        cx.simulate_resize(gpui::size(px(400.0), px(400.0)));
        cx.run_until_parked();
        let (queue, transcript) =
            view.read_with(cx, |view, _| (view.queue.clone(), view.transcript.clone()));
        for delta in [-80.0, -10_000.0, -80.0, 10_000.0, 80.0] {
            cx.simulate_event(gpui::ScrollWheelEvent {
                position: point(px(200.0), px(300.0)),
                delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(delta))),
                ..Default::default()
            });
            cx.run_until_parked();
            assert_eq!(
                transcript.offset().y,
                px(0.0),
                "queue wheel leaked to transcript"
            );
            if delta == -80.0 {
                assert!(queue.offset().y < px(0.0), "the queue must still scroll");
            }
        }
        view.update(cx, |view, cx| {
            view.count = 2;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: point(px(200.0), px(370.0)),
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
            ..Default::default()
        });
        cx.run_until_parked();
        assert_eq!(transcript.offset().y, px(0.0));
        assert_eq!(queue.max_offset().y, px(0.0));
    }
}

#[cfg(test)]
mod appshot_edit_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn restoring_displaced_draft_keeps_appshots_and_new_captures(cx: &mut TestAppContext) {
        let state = cx.new(|_| crate::state::AppState::new());
        let composer = cx.new(|cx| Composer::new(state, cx));
        composer.update(cx, |composer, cx| {
            let original = crate::appshots::tests::shot();
            let mut during_save = original.clone();
            during_save.id = "during-save".into();
            composer.queue_edit_draft = Some(("draft".into(), vec![], vec![original]));
            composer.editing_queued = Some("row".into());
            composer.queue_edit_finishing = true;
            composer.stage_appshot(during_save, cx);
            assert!(composer.staged_appshots().is_empty());
            composer.clear_queue_edit_local(cx);
            assert_eq!(composer.input.read(cx).text(), "draft");
            assert_eq!(composer.staged_appshots().len(), 2);
            assert_eq!(composer.staged_appshots()[1].id, "during-save");
            assert!(composer.editing_queued.is_none());
        });
    }
}
