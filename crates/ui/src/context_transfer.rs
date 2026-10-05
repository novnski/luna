//! Explicit, host-previewed conversation context transfers (harness evolution
//! slice 2): a provider handoff, a fork's findings sent back to its source,
//! and their receipts. The owning host builds the preview and the exact text;
//! the UI never builds, edits or resends transferred text, and applying a
//! transfer never starts or interrupts a turn.

use gpui::{App, SharedString, div, prelude::*, px};
use luna_proto::{
    CommandReceipt, CommandReceiptStatus, ContextCapacitySource, ContextTransferKind,
    ContextTransferPreview, ContextTransferReceipt, ContextTransferRequest, ContextTransferStatus,
};

use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;

/// Host capability for the transfer RPCs (older hosts keep their existing
/// bounded handoff path; nothing is previewed locally for them).
pub(crate) const CAPABILITY: &str = luna_proto::capabilities::CONTEXT_TRANSFER_V1;
const PREVIEW_METHOD: &str = luna_rpc::methods::PREVIEW_CONTEXT_TRANSFER;
const LIST_METHOD: &str = luna_rpc::methods::LIST_CONTEXT_TRANSFERS;
const READ_ITEMS_METHOD: &str = luna_rpc::methods::READ_CONTEXT_TRANSFER_ITEMS;

/// Whether `task_id`'s owning host offers explicit context transfers.
pub(crate) fn supported(state: &AppState, task_id: &str) -> bool {
    state.chat_host_supports(task_id, CAPABILITY)
}

/// A host preview being prepared or reviewed.
#[derive(Clone)]
pub(crate) enum PreviewState {
    Loading,
    Ready(Box<ContextTransferPreview>),
    Error(String),
    /// The host refused the reviewed basis, or the conversation, target or
    /// connection changed: a new preview is required before anything is sent.
    Stale(String),
}

/// What applying the reviewed preview produced.
#[derive(Clone, PartialEq)]
pub(crate) enum ApplyState {
    Idle,
    Applying {
        command_id: String,
    },
    /// Stored for the destination's next explicit message.
    Applied,
    /// The host's answer was lost: never resent; only its receipt is re-read.
    Unknown {
        command_id: String,
        checking: bool,
    },
    Failed(String),
}

/// One explicit transfer under review: the request, the connection it was
/// previewed on, the host's preview and the apply outcome.
#[derive(Clone)]
pub(crate) struct TransferFlow {
    pub request: ContextTransferRequest,
    pub engine_generation: u64,
    pub preview: PreviewState,
    pub apply: ApplyState,
    /// "Exactly what will be sent" expanded.
    pub show_text: bool,
    /// Apply commands sent for this flow (QA: Check status never adds one).
    pub apply_attempts: u32,
}

impl TransferFlow {
    pub(crate) fn new(request: ContextTransferRequest, engine_generation: u64) -> Self {
        Self {
            request,
            engine_generation,
            preview: PreviewState::Loading,
            apply: ApplyState::Idle,
            show_text: false,
            apply_attempts: 0,
        }
    }

    /// The reviewed preview, when it may still be applied.
    pub(crate) fn ready(&self) -> Option<&ContextTransferPreview> {
        match &self.preview {
            PreviewState::Ready(preview) => Some(preview),
            _ => None,
        }
    }

    /// An apply is in flight or its outcome is unknown: the flow (and its
    /// retained command id) must not be replaced or refreshed.
    pub(crate) fn busy(&self) -> bool {
        matches!(
            self.apply,
            ApplyState::Applying { .. } | ApplyState::Unknown { .. }
        )
    }

    /// Whether the primary action may run now.
    pub(crate) fn can_apply(&self) -> bool {
        self.ready().is_some() && matches!(self.apply, ApplyState::Idle | ApplyState::Failed(_))
    }

    /// Settle a preview response for this flow (refused for another
    /// connection: the caller checks `engine_generation` first).
    pub(crate) fn install_preview(&mut self, result: Result<ContextTransferPreview, String>) {
        self.preview = match result {
            Ok(preview) => PreviewState::Ready(Box::new(preview)),
            Err(error) => PreviewState::Error(error),
        };
    }

    /// Settle an apply result. A Conflict asks for a new preview (the draft
    /// and every other choice stay as they are).
    pub(crate) fn install_apply(&mut self, command_id: String, result: ApplyOutcome) {
        match result {
            ApplyOutcome::Applied => self.apply = ApplyState::Applied,
            ApplyOutcome::Conflict(message) => {
                self.apply = ApplyState::Idle;
                self.preview = PreviewState::Stale(message);
            }
            ApplyOutcome::Failed(message) => self.apply = ApplyState::Failed(message),
            ApplyOutcome::Unknown => {
                self.apply = ApplyState::Unknown {
                    command_id,
                    checking: false,
                }
            }
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub(crate) fn readback(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.request.kind,
            "sourceTaskId": self.request.source_task_id,
            "targetTaskId": self.request.target_task_id,
            "targetProvider": self.request.target_config.as_ref().map(|config| config.harness),
            "engineGeneration": self.engine_generation,
            "preview": match &self.preview {
                PreviewState::Loading => serde_json::json!({"state": "loading"}),
                PreviewState::Error(error) => serde_json::json!({"state": "error", "message": error}),
                PreviewState::Stale(message) => serde_json::json!({"state": "stale", "message": message}),
                PreviewState::Ready(preview) => serde_json::json!({
                    "state": "ready",
                    "previewId": preview.preview_id,
                    "included": preview.included.len(),
                    "omitted": preview.omitted.len(),
                    "alreadyDelivered": preview.already_delivered.len(),
                    "delta": preview.delta,
                    "capacitySource": preview.budget.capacity_source,
                    "summary": preview_summary(preview),
                    "budget": budget_line(preview),
                    "textBytes": preview.text.len(),
                }),
            },
            "apply": match &self.apply {
                ApplyState::Idle => serde_json::json!({"state": "idle"}),
                ApplyState::Applying { .. } => serde_json::json!({"state": "applying"}),
                ApplyState::Applied => serde_json::json!({"state": "applied"}),
                ApplyState::Unknown { checking, command_id } => serde_json::json!({"state": "unknown", "checking": checking, "commandId": command_id}),
                ApplyState::Failed(message) => serde_json::json!({"state": "failed", "message": message}),
            },
            "applyAttempts": self.apply_attempts,
        })
    }
}

/// One omitted (or included) item read back from the source on request.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferItemView {
    pub message_id: String,
    pub role: String,
    pub available: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// The UTF-8 byte range of the message this text covers.
    #[serde(default)]
    pub start_byte: Option<u64>,
    #[serde(default)]
    pub end_byte: Option<u64>,
    /// Where the rest of this message continues (Read more), when it does.
    #[serde(default)]
    pub next_text_offset: Option<u64>,
}

/// One omitted item in Details, with the segments read so far.
#[derive(Debug, Clone)]
pub(crate) struct DetailItem {
    pub view: TransferItemView,
    /// Each appended segment's start byte, in order.
    pub segment_starts: Vec<u64>,
    /// The first segment's text (later segments only ever extend it).
    #[cfg(feature = "appshots-fixture")]
    pub first_len: usize,
    pub reading: bool,
    pub error: Option<String>,
}

impl DetailItem {
    pub(crate) fn new(view: TransferItemView) -> Self {
        Self {
            segment_starts: vec![view.start_byte.unwrap_or(0)],
            #[cfg(feature = "appshots-fixture")]
            first_len: view.text.as_deref().map_or(0, str::len),
            view,
            reading: false,
            error: None,
        }
    }

    /// Whether more of this message can be read now (exactly from the
    /// cursor the host returned).
    pub(crate) fn can_read_more(&self) -> bool {
        self.view.available
            && self.view.truncated
            && self.view.next_text_offset.is_some()
            && !self.reading
    }

    /// Append the next segment of this same message, read from `offset`. A
    /// different message, a segment not starting at `offset`, or an
    /// unavailable continuation appends nothing; read text is kept.
    pub(crate) fn append(&mut self, offset: u64, next: TransferItemView) {
        self.reading = false;
        if next.message_id != self.view.message_id {
            self.error = Some("The source returned a different message.".into());
            return;
        }
        if !next.available {
            self.view.next_text_offset = None;
            self.error = Some(
                next.reason
                    .map(|reason| format!("The rest is unavailable: {reason}"))
                    .unwrap_or_else(|| "The rest of this message is unavailable.".into()),
            );
            return;
        }
        // Append only a segment the host positions exactly: it starts at the
        // requested cursor, ends after exactly its UTF-8 bytes, and (when
        // more remains) points further on. Anything else adds nothing.
        let text = next.text.unwrap_or_default();
        let end = offset + text.len() as u64;
        let positioned = next.start_byte == Some(offset)
            && next.end_byte == Some(end)
            && !text.is_empty()
            && if next.truncated {
                next.next_text_offset
                    .is_some_and(|cursor| cursor > offset && cursor == end)
            } else {
                true
            };
        if !positioned {
            self.error = Some(
                "The source didn't confirm where this part belongs, so it wasn't added.".into(),
            );
            return;
        }
        self.view.text = Some(self.view.text.take().unwrap_or_default() + &text);
        self.view.truncated = next.truncated;
        self.view.end_byte = next.end_byte;
        self.view.next_text_offset = next.next_text_offset;
        self.segment_starts.push(offset);
        self.error = None;
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferItemsPage {
    items: Vec<TransferItemView>,
    #[serde(default)]
    next_offset: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferList {
    items: Vec<ContextTransferReceipt>,
}

/// A receipt's Details: the compact receipt plus omitted source items read on
/// request (unavailable after the source was deleted or rewritten).
#[derive(Clone)]
pub(crate) struct TransferDetails {
    pub task_id: String,
    pub engine_generation: u64,
    pub receipt: ContextTransferReceipt,
    pub items: Vec<DetailItem>,
    pub next_offset: Option<u64>,
    pub loading: bool,
    pub checking: bool,
    pub error: Option<String>,
}

impl TransferDetails {
    pub(crate) fn new(
        task_id: String,
        engine_generation: u64,
        receipt: ContextTransferReceipt,
    ) -> Self {
        Self {
            task_id,
            engine_generation,
            receipt,
            items: Vec::new(),
            next_offset: Some(0),
            loading: false,
            checking: false,
            error: None,
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub(crate) fn readback(&self, state: &AppState) -> serde_json::Value {
        serde_json::json!({
            "transferId": self.receipt.id,
            "kind": self.receipt.kind,
            "status": self.receipt.status,
            "line": receipt_line(state, &self.receipt, &self.task_id),
            "items": self.items.iter().map(|item| {
                let text = item.view.text.as_deref().unwrap_or("");
                serde_json::json!({
                    "messageId": item.view.message_id,
                    "role": item.view.role,
                    "available": item.view.available,
                    "truncated": item.view.truncated,
                    // Never "full" while more of the message remains.
                    "label": if item.view.truncated { "excerpt" } else { "full" },
                    "textBytes": text.len(),
                    "nextTextOffset": item.view.next_text_offset,
                    "segments": item.segment_starts.len(),
                    "segmentStarts": item.segment_starts,
                    "prefixKept": text.len() >= item.first_len,
                    "reading": item.reading,
                    "error": item.error,
                    "reason": item.view.reason,
                })
            }).collect::<Vec<_>>(),
            "nextOffset": self.next_offset,
            "loading": self.loading,
            "checking": self.checking,
            "error": self.error,
        })
    }
}

/// Omitted source items of a transfer, one page (read-only).
pub(crate) async fn read_items(
    engine: EngineHandle,
    task_id: String,
    transfer_id: String,
    offset: u64,
) -> Result<(Vec<TransferItemView>, Option<u64>), String> {
    let page: TransferItemsPage = engine
        .client()
        .call_as(
            READ_ITEMS_METHOD,
            serde_json::json!({
                "taskId": task_id,
                "transferId": transfer_id,
                "offset": offset,
                "limit": 20,
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok((page.items, page.next_offset))
}

/// The rest of one omitted message, from the host's `next_text_offset`
/// (read-only; at most one bounded segment).
pub(crate) async fn read_item_segment(
    engine: EngineHandle,
    task_id: String,
    transfer_id: String,
    message_id: String,
    text_offset: u64,
) -> Result<TransferItemView, String> {
    let page: TransferItemsPage = engine
        .client()
        .call_as(
            READ_ITEMS_METHOD,
            serde_json::json!({
                "taskId": task_id,
                "transferId": transfer_id,
                "messageId": message_id,
                "textOffset": text_offset,
                "offset": 0,
                "limit": 1,
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    page.items
        .into_iter()
        .find(|item| item.message_id == message_id)
        .ok_or_else(|| "The source did not return this message.".to_string())
}

/// The task's recent receipts as its host lists them now (Check status).
pub(crate) async fn list_receipts(
    engine: EngineHandle,
    task_id: String,
) -> Result<Vec<ContextTransferReceipt>, String> {
    let list: TransferList = engine
        .client()
        .call_as(
            LIST_METHOD,
            serde_json::json!({ "taskId": task_id, "limit": 20 }),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(list.items)
}

/// The host's settled answer to an apply command.
pub(crate) enum ApplyOutcome {
    Applied,
    Conflict(String),
    Failed(String),
    Unknown,
}

/// Ask the owning host for a preview (read-only: nothing is created, written
/// or sent to a provider).
pub(crate) async fn request_preview(
    engine: EngineHandle,
    request: ContextTransferRequest,
) -> Result<ContextTransferPreview, String> {
    let params = serde_json::to_value(&request).map_err(|error| error.to_string())?;
    engine
        .client()
        .call_as::<ContextTransferPreview>(PREVIEW_METHOD, params)
        .await
        .map_err(|error| error.to_string())
}

/// Apply exactly the reviewed preview on its destination, fenced by the
/// destination basis the host reviewed. A lost answer is `Unknown` (the
/// command's receipt is re-read, never resent).
pub(crate) async fn apply_preview(
    engine: EngineHandle,
    preview: ContextTransferPreview,
    command_id: String,
) -> ApplyOutcome {
    match crate::task_commands::apply_context_transfer(&engine, &preview, command_id).await {
        Ok(receipt) => outcome(&receipt),
        Err(_) => ApplyOutcome::Unknown,
    }
}

/// Re-read an apply command's receipt (Check status). `None`: still unknown.
pub(crate) async fn check_apply(engine: EngineHandle, command_id: String) -> Option<ApplyOutcome> {
    match crate::task_commands::read_receipt(&engine, &command_id).await {
        Ok(Some(receipt)) if receipt.status != CommandReceiptStatus::Pending => {
            Some(outcome(&receipt))
        }
        _ => None,
    }
}

fn outcome(receipt: &CommandReceipt) -> ApplyOutcome {
    match receipt.status {
        CommandReceiptStatus::Applied => ApplyOutcome::Applied,
        CommandReceiptStatus::Pending => ApplyOutcome::Unknown,
        CommandReceiptStatus::Conflict => ApplyOutcome::Conflict(
            "The conversation changed since this preview. Refresh it to review what would be sent now."
                .into(),
        ),
        _ => ApplyOutcome::Failed(crate::task_commands::failure_message(
            receipt,
            "The context transfer was not saved.",
        )),
    }
}

pub(crate) fn kind_label(kind: ContextTransferKind) -> &'static str {
    match kind {
        ContextTransferKind::ProviderHandoff => "Provider handoff",
        ContextTransferKind::PortableFork => "Context copy",
        ContextTransferKind::ForkMerge => "Findings",
    }
}

pub(crate) fn status_label(status: ContextTransferStatus) -> &'static str {
    match status {
        ContextTransferStatus::Planned => "Waiting for the next message",
        ContextTransferStatus::Delivered => "Delivered",
        ContextTransferStatus::Unknown => "Delivery unknown. It will not be resent.",
        ContextTransferStatus::Failed => "Failed",
        ContextTransferStatus::Consumed => "Used",
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// "6 messages included · 8 left out · 2 already delivered".
pub(crate) fn preview_summary(preview: &ContextTransferPreview) -> String {
    let mut parts = vec![format!(
        "{} included",
        plural(preview.included.len(), "message", "messages")
    )];
    if !preview.omitted.is_empty() {
        parts.push(format!("{} left out", preview.omitted.len()));
    }
    if !preview.already_delivered.is_empty() {
        parts.push(format!(
            "{} already delivered",
            preview.already_delivered.len()
        ));
    }
    parts.join(" · ")
}

/// The budget in words: an observed destination context, or the explicit
/// conservative allowance for an unknown one (never called a known capacity).
pub(crate) fn budget_line(preview: &ContextTransferPreview) -> String {
    let budget = &preview.budget;
    let estimate = format!(
        "About {} tokens ({:.1} KB)",
        preview.estimated_tokens,
        preview.text.len() as f64 / 1024.0
    );
    match (budget.capacity_source, budget.capacity_tokens) {
        (ContextCapacitySource::ObservedContext, Some(capacity)) => format!(
            "{estimate}. The destination reports a {capacity}-token context{}; {} tokens are kept for instructions, your next message and the reply.",
            budget
                .used_tokens
                .map(|used| format!(" with {used} in use"))
                .unwrap_or_default(),
            budget.reserved_tokens + budget.prompt_tokens
        ),
        _ => format!(
            "{estimate}. The destination's context size is unknown, so LUNA uses a conservative allowance of {:.0} KB.",
            budget.max_bytes as f64 / 1024.0
        ),
    }
}

/// A receipt as one line, from `perspective`'s side ("Findings from X" on
/// the destination, "Findings sent to Y" on the source). A source or
/// destination no longer in the chat list reads as deleted.
pub(crate) fn receipt_line(
    state: &AppState,
    receipt: &ContextTransferReceipt,
    perspective: &str,
) -> String {
    let named = |basis: &luna_proto::ContextTransferBasis| {
        let title = if basis.title.trim().is_empty() {
            "Untitled chat".to_string()
        } else {
            basis.title.clone()
        };
        if state.chats.iter().any(|chat| chat.id == basis.task_id) {
            title
        } else {
            format!("{title} (deleted)")
        }
    };
    let direction = if receipt.target.task_id == perspective {
        format!("from {}", named(&receipt.source))
    } else {
        format!("sent to {}", named(&receipt.target))
    };
    format!(
        "{} {direction} · {} · {}",
        kind_label(receipt.kind),
        plural(receipt.included_message_ids.len(), "message", "messages"),
        status_label(receipt.status)
    )
}

/// The preview body shared by the provider switch panel and the findings
/// dialog: what the host will send, what it leaves out, the budget, notes,
/// and the exact text on request. Bounded heights keep long content inside
/// the opaque card.
pub(crate) fn render_preview_body(
    theme: &Theme,
    flow: &TransferFlow,
    on_toggle_text: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
) -> gpui::Div {
    let muted = theme.readable_muted(0.7);
    let small = |text: SharedString, color: gpui::Hsla| {
        div()
            .text_size(crate::typography::ui_rems(11.0))
            .line_height(px(16.0))
            .text_color(color)
            .child(text)
    };
    let body = div().flex().flex_col().gap(px(6.0));
    let preview = match &flow.preview {
        PreviewState::Loading => {
            return body.child(small("Preparing the preview on the host…".into(), muted));
        }
        PreviewState::Error(error) => {
            return body.child(small(
                format!("Couldn't prepare the preview: {error}").into(),
                theme.readable_tone(theme.danger_muted, 1.0),
            ));
        }
        PreviewState::Stale(message) => {
            return body.child(small(message.clone().into(), theme.warning));
        }
        PreviewState::Ready(preview) => preview,
    };
    let excerpt = |text: &str| {
        let line = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        let mut short: String = line.chars().take(110).collect();
        if line.chars().count() > 110 || text.lines().count() > 1 {
            short.push('…');
        }
        short
    };
    let included = preview.included.iter().map(|item| {
        div()
            .flex()
            .flex_row()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(11.0))
            .line_height(px(16.0))
            .child(
                div()
                    .flex_none()
                    .w(px(64.0))
                    .text_color(muted)
                    .child(SharedString::from(item.role.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(SharedString::from(format!(
                        "{}{}{}",
                        if item.start_byte > 0 {
                            "(continued) "
                        } else {
                            ""
                        },
                        excerpt(&item.text),
                        if item.truncated { " (shortened)" } else { "" }
                    ))),
            )
    });
    let omitted = preview
        .omitted
        .iter()
        .map(|item| small(format!("{} · {}", item.role, item.reason).into(), muted));
    let mut body = body
        .child(small(preview_summary(preview).into(), theme.text))
        .child(small(budget_line(preview).into(), muted));
    if preview.delta {
        body = body.child(small(
            "Only messages this destination has not received before.".into(),
            muted,
        ));
    }
    for note in &preview.notes {
        body = body.child(small(note.clone().into(), muted));
    }
    body = body.child(
        div()
            .id("transfer-included")
            .max_h(px(120.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(included),
    );
    if !preview.omitted.is_empty() {
        body = body.child(small("Left out:".into(), muted)).child(
            div()
                .id("transfer-omitted")
                .max_h(px(72.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(omitted),
        );
    }
    body = body.child(
        div()
            .id("transfer-show-text")
            .role(gpui::Role::Button)
            .aria_label(if flow.show_text {
                "Hide the exact text"
            } else {
                "Show exactly what will be sent"
            })
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.accent)
            .on_click(on_toggle_text)
            .child(if flow.show_text {
                "Hide the exact text"
            } else {
                "Show exactly what will be sent"
            }),
    );
    if flow.show_text {
        body = body.child(
            div()
                .id("transfer-exact-text")
                .max_h(px(180.0))
                .overflow_y_scroll()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(theme.ink(0.04))
                .text_size(crate::typography::ui_rems(11.0))
                .line_height(px(16.0))
                .text_color(theme.text)
                .child(SharedString::from(preview.text.clone())),
        );
    }
    body
}

/// The apply state's line under the preview (unknown says it will not be
/// resent; failures name the host's reason).
pub(crate) fn apply_line(theme: &Theme, flow: &TransferFlow) -> Option<gpui::Stateful<gpui::Div>> {
    let (text, color): (SharedString, gpui::Hsla) = match &flow.apply {
        ApplyState::Idle | ApplyState::Applied => return None,
        ApplyState::Applying { .. } => ("Saving…".into(), theme.readable_muted(0.7)),
        ApplyState::Unknown { checking: true, .. } => {
            ("Checking the status…".into(), theme.readable_muted(0.7))
        }
        ApplyState::Unknown { .. } => (
            "LUNA couldn't confirm whether this was saved. It will not be sent again; check its status instead."
                .into(),
            theme.warning,
        ),
        ApplyState::Failed(message) => (
            message.clone().into(),
            theme.readable_tone(theme.danger_muted, 1.0),
        ),
    };
    Some(
        div()
            .id("transfer-apply-state")
            .text_size(crate::typography::ui_rems(11.0))
            .line_height(px(16.0))
            .text_color(color)
            .child(text),
    )
}
