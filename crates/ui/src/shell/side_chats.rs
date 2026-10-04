//! Starting a side thread: a persistent fork of a conversation.
//!
//! A side thread begins as a fork (or a frozen context snapshot) of its
//! source and is then an ordinary thread: the engine publishes it with its
//! real project and its source (`sideChatParentId`), and the UI manages it
//! like any chat (workbench tab or Main, close/reopen, drafts, send, stop,
//! model, approvals, recovery, archive, pin, settle, delete). The source is
//! a link, never a lifetime: there is no End, expiry, lease, unload or
//! read-only "ended" state. `sideChatState` is legacy metadata only.
//!
//! This module only creates forks. A create is idempotent by request id: an
//! outcome the engine may have completed (a lost connection, an unresolved
//! receipt) keeps its id, so Retry repeats the same request and never makes
//! a second fork. A created fork opens as a workbench tab under its source
//! (or, without the workbench, as the selected chat).

use super::*;
use luna_engine::side_chats::SideChatCreated;
use serde_json::{Value, json};

/// A create the engine may have completed: the connection or the task was
/// lost, or the engine reports the creation unresolved (it will not replay
/// it). Its request id must survive for an idempotent retry.
fn create_outcome_unknown<E>(result: &Result<Result<Value, luna_rpc::RpcError>, E>) -> bool {
    match result {
        Err(_) => true,
        Ok(Err(luna_rpc::RpcError::Transport(_) | luna_rpc::RpcError::Closed)) => true,
        Ok(Err(luna_rpc::RpcError::Failed(message))) => {
            let message = message.to_lowercase();
            [
                "unresolved",
                "will not be replayed",
                "outcome is unknown",
                "outcome may be unknown",
                "saved receipt",
            ]
            .iter()
            .any(|marker| message.contains(marker))
        }
        _ => false,
    }
}

/// `id`'s source when it is a Side chat (it carries that chat's context):
/// a service side chat's own row, else the authoritative lineage the chats
/// watch publishes (`forkSourceTaskId`, which also covers generic history
/// forks). The source may since have been deleted: the chat stays a thread.
pub(super) fn fork_source<'a>(state: &'a AppState, id: &str) -> Option<&'a str> {
    state
        .side_chat_rows
        .get(id)
        .map(|row| row.parent_id.as_str())
        .or_else(|| state.fork_source_rows.get(id).map(String::as_str))
        .filter(|source| *source != id)
}

/// A chat the side chat service owns (its tab carries a side-parent hint
/// and its service rows). A generic history fork is not one: it opens as an
/// ordinary chat tab, and no service-only call is made for it.
pub(super) fn is_service_side_chat(state: &AppState, id: &str) -> bool {
    state.side_chat_rows.contains_key(id)
}

/// Whether `id` is a fork (a side thread) at all.
pub(super) fn is_fork(state: &AppState, id: &str) -> bool {
    fork_source(state, id).is_some()
}

/// A materialized side thread: the engine publishes its ordinary chat row,
/// so it takes the ordinary views, composers and commands, whatever its
/// legacy `sideChatState` says.
pub(super) fn side_chat_first_class(state: &AppState, side_id: &str) -> bool {
    state.chats.iter().any(|chat| chat.id == side_id)
}

/// What the Related chats header shows about starting a side thread.
#[derive(Debug, Clone, Default)]
pub(super) struct SideCreateSnapshot {
    pub busy: bool,
    pub operation: Option<&'static str>,
    pub error: Option<String>,
    /// An unresolved create (its snapshot flag): Retry repeats that request.
    pub pending_create: Option<bool>,
    /// A plain fork failed: offer starting from recent context instead.
    pub snapshot_alternative: bool,
}

pub(super) struct SideChatPanel {
    state: Entity<AppState>,
    engine: Option<crate::state::EngineHandle>,
    engine_generation: u64,
    /// The selected chat this panel belongs to (the source may be a tab).
    owner_chat: Option<String>,
    parent: String,
    /// The quoted message a fork starts from, when it came from one:
    /// `(message id, anchor text)`.
    origin: Option<(String, String)>,
    /// A create whose outcome is unknown keeps its request id:
    /// `(request id, snapshot)`.
    pending_create: Option<(String, bool)>,
    busy: bool,
    operation_label: Option<&'static str>,
    error: Option<String>,
    snapshot_alternative: bool,
    /// A fork this panel created, not yet opened.
    created: Option<String>,
    task: Option<Task<()>>,
}

impl SideChatPanel {
    pub(super) fn new(
        state: Entity<AppState>,
        parent: String,
        origin: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = state.read(cx).engine().cloned();
        let engine_generation = state.read(cx).engine_generation();
        let owner_chat = state.read(cx).selected_chat.clone();
        let quoted = origin.is_some();
        let mut this = Self {
            state,
            engine,
            engine_generation,
            owner_chat,
            parent,
            origin,
            pending_create: None,
            busy: false,
            operation_label: None,
            error: None,
            snapshot_alternative: false,
            created: None,
            task: None,
        };
        // "Fork from here" on a message starts at once.
        if quoted {
            this.create(false, cx);
        }
        this
    }

    /// The selected chat and engine this panel was made for are current.
    pub(super) fn is_current(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        state.engine_generation() == self.engine_generation
            && state.selected_chat == self.owner_chat
    }

    /// The chat this panel starts side threads from (their source).
    pub(super) fn parent_id(&self) -> &str {
        &self.parent
    }

    pub(super) fn is_busy(&self) -> bool {
        self.busy
    }

    pub(super) fn snapshot(&self) -> SideCreateSnapshot {
        SideCreateSnapshot {
            busy: self.busy,
            operation: self.operation_label,
            error: self.error.clone(),
            pending_create: self.pending_create.as_ref().map(|(_, snapshot)| *snapshot),
            snapshot_alternative: self.snapshot_alternative,
        }
    }

    /// The fork this panel created and has not handed over yet.
    pub(super) fn created(&self) -> Option<&str> {
        self.created.as_deref()
    }

    /// Hand the created fork over for opening, once.
    pub(super) fn take_created(&mut self) -> Option<String> {
        self.created.take()
    }

    /// Start (or, while an earlier outcome is unknown, retry) a side thread:
    /// a fork, or with `snapshot` a frozen context snapshot.
    pub(super) fn create(&mut self, snapshot: bool, cx: &mut Context<Self>) {
        if self.busy || !self.is_current(cx) {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            self.error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        // A retry after an unknown outcome repeats the same request, so the
        // engine returns the thread it already created.
        let request_id = match &self.pending_create {
            Some((id, pending)) if *pending == snapshot => id.clone(),
            _ => uuid::Uuid::new_v4().to_string(),
        };
        self.pending_create = Some((request_id.clone(), snapshot));
        let params = json!({
            "parentId": self.parent,
            "requestId": request_id,
            "originMessageId": self.origin.as_ref().map(|origin| &origin.0),
            "anchorText": self.origin.as_ref().map(|origin| &origin.1),
            "forceSnapshot": snapshot,
        });
        self.busy = true;
        self.operation_label = Some("Creating side chat…");
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            // Detached: dropping this panel ends only the wait, never a
            // create the engine accepted. Creation waits unbounded (the
            // engine bounds provider work).
            let handle = cx.update(|cx| Tokio::handle(cx));
            let join = handle.spawn(async move {
                engine
                    .client()
                    .call(methods::CREATE_SIDE_CHAT, params)
                    .await
            });
            let result = Tokio::spawn(cx, async move {
                // A lost task is an unknown outcome, like a lost connection.
                join.await
                    .map_err(|error| luna_rpc::RpcError::Transport(error.to_string()))?
            })
            .await;
            this.update(cx, |panel, cx| {
                if !panel.is_current(cx) {
                    return;
                }
                panel.busy = false;
                panel.operation_label = None;
                // Keep the request id only while the outcome is unknown: a
                // retry is then the same idempotent request, never a new fork.
                if !create_outcome_unknown(&result) {
                    panel.pending_create = None;
                }
                let created = match result {
                    Ok(Ok(value)) => serde_json::from_value::<SideChatCreated>(value)
                        .map_err(|error| error.to_string()),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                match created {
                    Ok(created) => {
                        panel.snapshot_alternative = false;
                        panel.origin = None;
                        panel.created = Some(created.meta.id);
                    }
                    Err(error) => {
                        panel.error = Some(error);
                        panel.snapshot_alternative = !snapshot;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Give up an unresolved create: the user's explicit choice to start a
    /// different side thread (the unresolved one may still appear).
    pub(super) fn forget_pending(&mut self) {
        if !self.busy {
            self.pending_create = None;
        }
    }
}

impl Render for SideChatPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Side threads are listed by Related chats and open as ordinary
        // views; the creator itself draws nothing.
        Empty
    }
}

impl Shell {
    /// Drop the creator. A create already sent completes in the engine, and
    /// its fork appears in the lists; nothing here ends a thread.
    pub(super) fn release_side_chat(&mut self, _cx: &mut Context<Self>) {
        self.side_chat_panel = None;
        self.side_chat_observation = None;
    }

    pub(super) fn create_side_chat_panel(
        &mut self,
        origin: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        self.create_side_chat_panel_for(self.active_chat.clone(), origin, cx);
    }

    pub(super) fn create_side_chat_panel_for(
        &mut self,
        parent: String,
        origin: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let panel = cx.new(|cx| SideChatPanel::new(state, parent, origin, cx));
        self.side_chat_observation = Some(cx.observe(&panel, |shell, _, cx| {
            shell.open_created_side_chat(cx);
            cx.notify();
        }));
        self.side_chat_panel = Some(panel);
        self.open_created_side_chat(cx);
    }

    /// A fork the creator made opens once its ordinary row is published: as
    /// a tab under its source in the workbench, or (no workbench) as the
    /// selected chat. Checked on creator and state changes.
    pub(super) fn open_created_side_chat(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.side_chat_panel.clone() else {
            return;
        };
        let Some(id) = panel.read(cx).created().map(str::to_string) else {
            return;
        };
        if !side_chat_first_class(self.state.read(cx), &id) {
            return;
        }
        panel.update(cx, |panel, _| panel.take_created());
        self.open_side_chat_in_pane(id, cx);
    }

    /// The creator for the selected chat, made on first use. One left on
    /// another source is replaced once nothing is in flight in it.
    pub(super) fn ensure_side_chat_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<SideChatPanel> {
        let stale = self.side_chat_panel.as_ref().is_some_and(|panel| {
            let panel = panel.read(cx);
            panel.parent_id() != self.active_chat
                && !panel.is_busy()
                && panel.snapshot().pending_create.is_none()
                && panel.created().is_none()
        });
        if stale {
            self.release_side_chat(cx);
        }
        if self.side_chat_panel.is_none() {
            self.create_side_chat_panel(None, cx);
        }
        self.side_chat_panel.clone().expect("created above")
    }

    /// Open a side thread: a workbench tab under its source, or the
    /// selected chat without the workbench. It is an ordinary thread.
    pub(super) fn open_side_chat_in_pane(&mut self, id: String, cx: &mut Context<Self>) {
        // Only a service side chat's tab carries the side-parent hint; a
        // generic history fork opens as an ordinary chat tab.
        let service = is_service_side_chat(self.state.read(cx), &id);
        if !self.open_related_in_workbench(id.clone(), service, cx) {
            self.open_chat(id, cx);
        }
    }

    /// The user's New side chat (agents never start side chats).
    pub(super) fn start_side_chat(&mut self, snapshot: bool, cx: &mut Context<Self>) {
        self.related_focus = super::related_chats::RelatedKind::Side;
        let panel = self.ensure_side_chat_panel(cx);
        panel.update(cx, |panel, cx| panel.create(snapshot, cx));
    }

    /// The user's explicit choice to start a different side chat while an
    /// earlier request is unresolved: its request id is dropped, so this is a
    /// new fork (the unresolved one may still appear in the list).
    pub(super) fn start_new_side_chat_instead(&mut self, cx: &mut Context<Self>) {
        self.related_focus = super::related_chats::RelatedKind::Side;
        let panel = self.ensure_side_chat_panel(cx);
        panel.update(cx, |panel, cx| {
            panel.forget_pending();
            panel.create(false, cx);
        });
    }

    /// "Use in parent": a side thread's answer goes into its source's own
    /// draft, wherever the source is open, for the user to review and send.
    pub(super) fn hand_side_result_to_source(
        &mut self,
        side_chat_id: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = fork_source(self.state.read(cx), side_chat_id).map(str::to_string)
        else {
            return;
        };
        let generation = self.state.read(cx).engine_generation();
        let composer = if self.active_chat == source {
            Some(self.composer.clone())
        } else {
            self.pane_composers.read(cx).get(&source)
        };
        let inserted = composer.is_some_and(|composer| {
            composer.update(cx, |composer, cx| {
                composer.insert_side_chat_result(&source, text, generation, cx)
            })
        });
        self.sidebar_notice = Some(SidebarNotice::status(if inserted {
            "Added to the parent draft. Review it and press Send."
        } else {
            "Parent draft is loading or being submitted. Try again when it is ready."
        }));
        cx.notify();
    }
}

/// The ordinary command a legacy device-local mark becomes. Archive intent
/// wins (a pin or settle on an archived chat is moot); then a pin, then a
/// settle.
fn legacy_mark_intent(marks: &crate::settings::SideChatMarks) -> Option<Value> {
    if marks.archived {
        Some(json!({ "kind": "setArchived", "archived": true }))
    } else if marks.pinned_at_ms.is_some() {
        Some(json!({ "kind": "setPinned", "pinned": true }))
    } else if marks.settled {
        Some(json!({ "kind": "setSettled", "settled": true }))
    } else {
        None
    }
}

/// Whether the fork's own row already shows what its legacy mark meant: the
/// readback that acknowledges the migration.
fn legacy_mark_applied(chat: &luna_proto::Chat, marks: &crate::settings::SideChatMarks) -> bool {
    if marks.archived {
        chat.archived
    } else if marks.pinned_at_ms.is_some() {
        chat.pinned_order.is_some()
    } else if marks.settled {
        chat.archived
            || chat.settlement_override.as_ref().is_some_and(|settlement| {
                settlement.disposition == luna_proto::SettlementDisposition::Settled
            })
    } else {
        true
    }
}

/// Run one task command and wait for its receipt to apply.
async fn apply_task_command(
    engine: &crate::state::EngineHandle,
    task_id: &str,
    device: &str,
    intent: Value,
) -> Result<(), String> {
    let receipt = crate::task_commands::execute(engine, task_id, intent)
        .await
        .map_err(|error| error.to_string())?;
    if receipt.status == luna_proto::CommandReceiptStatus::Applied {
        return Ok(());
    }
    if receipt.status.is_terminal() {
        return Err(crate::task_commands::failure_message(
            &receipt,
            "Could not update this side chat.",
        ));
    }
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let params = json!({ "commandId": receipt.command_id, "targetDeviceId": device });
        match engine
            .client()
            .call_as::<Option<luna_proto::CommandReceipt>>(
                methods::GET_TASK_COMMAND_RECEIPT,
                params,
            )
            .await
        {
            Ok(Some(receipt)) if receipt.status == luna_proto::CommandReceiptStatus::Applied => {
                return Ok(());
            }
            Ok(Some(receipt)) if receipt.status.is_terminal() => {
                return Err(crate::task_commands::failure_message(
                    &receipt,
                    "Could not update this side chat.",
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("LUNA could not confirm this side chat's flags yet.".into())
}

impl Shell {
    /// Side chats used to keep pin/settle/archive as device-local marks.
    /// A materialized fork takes the ordinary synced commands instead: each
    /// mark is sent as its command, and cleared only once the fork's own row
    /// reads back the same intent. A mark whose fork is not published yet,
    /// or whose command failed (retried on the next connection), is kept.
    pub(super) fn migrate_side_chat_marks(&mut self, cx: &mut Context<Self>) {
        if self.side_marks_migration.is_some() || self.settings.side_chat_marks.is_empty() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let generation = self.state.read(cx).engine_generation();
        if self.side_marks_attempted.0 != generation {
            self.side_marks_attempted = (generation, Default::default());
        }
        let mut acknowledged = Vec::new();
        let mut pending = Vec::new();
        {
            let state = self.state.read(cx);
            for (id, marks) in &self.settings.side_chat_marks {
                let Some(chat) = state.chats.iter().find(|chat| chat.id == *id) else {
                    continue;
                };
                if legacy_mark_applied(chat, marks) {
                    acknowledged.push(id.clone());
                } else if !self.side_marks_attempted.1.contains(id)
                    && let Some(intent) = legacy_mark_intent(marks)
                {
                    pending.push((id.clone(), chat.device_id.clone(), intent));
                }
            }
        }
        if !acknowledged.is_empty() {
            for id in &acknowledged {
                self.settings.side_chat_marks.remove(id);
            }
            self.schedule_save(cx);
        }
        if pending.is_empty() {
            return;
        }
        for (id, _, _) in &pending {
            self.side_marks_attempted.1.insert(id.clone());
        }
        self.side_marks_migration = Some(cx.spawn(async move |this, cx| {
            let mut failed = 0usize;
            for (id, device, intent) in pending {
                if let Err(error) = apply_task_command(&engine, &id, &device, intent).await {
                    tracing::warn!(side_chat = %id, %error, "side chat mark migration failed");
                    failed += 1;
                }
            }
            this.update(cx, |shell, cx| {
                shell.side_marks_migration = None;
                if failed > 0 {
                    shell.sidebar_notice = Some(SidebarNotice::transient_error(
                        "Some side chats could not take their pin, settle or archive yet. LUNA keeps them and tries again when it reconnects.",
                    ));
                }
                // Acknowledge whatever already reads back.
                shell.migrate_side_chat_marks(cx);
                cx.notify();
            })
            .ok();
        }));
    }
}

/// Headless QA drives the production side thread creator through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_side_chats(&mut self, cx: &mut Context<Self>) {
        self.add_work_surface(RightSurface::RelatedChats, cx);
        if self.side_chat_panel.is_none() {
            self.create_side_chat_panel(None, cx);
        }
    }

    /// `new` (a fork), `snapshot` (from recent context) or `instead` (a new
    /// fork while an earlier create is unresolved).
    pub fn fixture_side_chat(&mut self, action: &str, _text: &str, cx: &mut Context<Self>) {
        match action {
            "new" => self.start_side_chat(false, cx),
            "snapshot" => self.start_side_chat(true, cx),
            "instead" => self.start_new_side_chat_instead(cx),
            _ => {}
        }
    }

    pub fn fixture_side_chat_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        match &self.side_chat_panel {
            Some(panel) => {
                let panel = panel.read(cx);
                let snapshot = panel.snapshot();
                json!({
                    "parent": panel.parent_id(),
                    "busy": snapshot.busy,
                    "operation": snapshot.operation,
                    "error": snapshot.error,
                    "pendingCreate": snapshot.pending_create,
                    "snapshotAlternative": snapshot.snapshot_alternative,
                    "created": panel.created(),
                })
            }
            None => Value::Null,
        }
    }

    /// Use in parent, as the side thread's own transcript action emits it.
    pub fn fixture_side_result_to_source(
        &mut self,
        side_chat_id: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.hand_side_result_to_source(side_chat_id, text, cx);
    }
}
