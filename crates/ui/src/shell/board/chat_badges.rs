//! Ticket identity on chat surfaces (A10; contract §6, §7.3, §9; chat-model
//! §1–§5 and §8; automatic-review §2).
//!
//! Every chat surface shows what the engine derived, never a guess:
//! - **Badges** (`BoardChatBadges`): a ticket's working chat shows
//!   `Ticket LUN-42` in its header (with a separate workflow-status control)
//!   and a compact key on its sidebar row, side-chat tab and side-chat list
//!   row; a temporary reviewer shows `Reviewer · LUN-42`; a source discussion
//!   gets a "Related ticket" receipt, never the working badge. Meaning is
//!   carried by a glyph and text, never by colour alone.
//! - **Roles** (`BoardChatIdentity`): a main chat offers Side chat (Fresh or
//!   With context); a side chat offers only New side chat (a fresh sibling
//!   under the same main chat) and Hand off to a new main chat. Until the
//!   engine's identity loads, the published lineage stands in
//!   ([`local_role`]); the engine validates every creation either way.
//! - **Go to chat** ([`Shell::board_go_to_chat`], [`Shell::board_open_navigation`]):
//!   the entry point Board cards, the detail drawer, receipts, Activity and
//!   the palette call. It reopens a view of the exact chat (a main chat, or
//!   its main chat's workbench with the exact side chat selected, its tab
//!   reopened if it was closed) and never relaunches, resends, promotes or
//!   reparents anything.
//! - **Actions**: Convert this chat to a ticket (in place), Create ticket
//!   from this chat (save only), New side chat / Hand off from a side chat,
//!   workflow moves from the status control and the working chat's rename,
//!   which goes through the ticket so both titles change together.
//!
//! There is no Move here, reattach or change-parent control anywhere: a
//! ticket chat keeps its original parent (chat-model §8).
use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, Subscription, Task};
use luna_proto::{
    BoardChatBadgesRequest, BoardChatIdentityRequest, BoardChatRole, BoardColumn, BoardContextMode,
    BoardDestination, BoardError, BoardEventsRequest, BoardFollowUp, BoardItemChanges,
    BoardMoveRequest, BoardNavigationRequest, BoardNavigationTarget, BoardReadRequest, BoardScope,
    BoardUpdateRequest, CanonicalChatIdentity, ChatPermittedActions, ChatPurpose, ChatTicketBadge,
    HarnessId, TaskLinkRole, WorkItemEvent,
};

use super::api::{BoardApi, BoardCallError};
use super::model::{BoardModel, BoardModelEvent};
use crate::shell::{Shell, SidebarNotice, SidebarPage};
use crate::state::AppState;

pub(crate) mod dialogs;
#[cfg(feature = "appshots-fixture")]
mod fixture;
pub(crate) mod palette;
pub(crate) mod render;

/// Badge reads are chunked so a large workspace never sends one unbounded
/// request.
const BADGE_CHUNK: usize = 200;
/// How long a Go to chat waits for its main chat's workbench to load before
/// it opens the side chat on its own.
const REVEAL_TIMEOUT: Duration = Duration::from_secs(12);
/// A reveal re-asks the workbench at most this often while it waits.
const REVEAL_RETRY: Duration = Duration::from_millis(1500);
/// How long a reveal waits for the engine's own placement of a chat it has
/// just created before opening the tab itself.
const PLACEMENT_WAIT: Duration = Duration::from_millis(1500);
/// How often a waiting reveal checks again.
const REVEAL_POLL: Duration = Duration::from_millis(250);
/// A reveal asks the workbench at most this many times.
const REVEAL_ASKS: u8 = 3;
/// Recent board activity kept for the Activity view.
const ACTIVITY_KEPT: usize = 40;

/// Render one compact badge (`LUN-42`, or `Reviewer · LUN-42`) with its
/// accessible label and tooltip: the foundation entry point other Board
/// surfaces (cards, the detail drawer) use for a chat's ticket identity.
#[allow(dead_code)] // Called by the Board page and detail slices (A8/A9).
pub(crate) fn render_badge(
    badge: &ChatTicketBadge,
    theme: &crate::theme::Theme,
) -> gpui::AnyElement {
    use gpui::IntoElement as _;
    render::compact_badge(
        badge,
        format!("ticket-badge-{}-{}", badge.task_id, badge.work_item_id),
        theme,
    )
    .into_any_element()
}

/// A board call that this engine does not serve: an older engine, or a
/// slice that has not landed in this build. Nothing is shown for it.
pub(crate) fn board_unavailable(error: &BoardCallError) -> bool {
    matches!(
        error,
        BoardCallError::Unsupported | BoardCallError::Board(BoardError::NotImplemented { .. })
    )
}

/// A fresh client idempotency key (`[A-Za-z0-9_.:-]`), reused verbatim only
/// to repeat the same request after an uncertain answer.
pub(crate) fn new_request_id(scope: &str) -> String {
    format!("ui-{scope}-{}", uuid::Uuid::new_v4().simple())
}

/// A provider's name for tooltips.
pub(crate) fn harness_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Mock => "Mock",
        HarnessId::Cursor => "Cursor",
        HarnessId::Devin => "Devin",
        HarnessId::Grok => "Grok",
        HarnessId::Hermes => "Hermes",
        HarnessId::Pi => "Pi",
        HarnessId::Antigravity => "Antigravity",
        #[allow(unreachable_patterns)]
        _ => "Agent",
    }
}

/// `id`'s parent by its published lineage: a `ChildThread` parent, a side
/// chat's source row or a generic fork source.
fn lineage_parent(state: &AppState, id: &str) -> Option<String> {
    state
        .chats
        .iter()
        .find(|chat| chat.id == id)
        .and_then(crate::shell::placement::lineage_parent_id)
        .map(str::to_string)
        .or_else(|| crate::shell::side_chats::fork_source(state, id).map(str::to_string))
        .filter(|parent| parent != id)
}

/// The canonical role from the published lineage, for when the engine's
/// identity has not loaded: a side-chat row, `ChildThread` metadata or a
/// fork source make a side chat, whose main parent is its first main
/// ancestor (a legacy nested record walks up). Never the tab, layout or
/// title. Hiding a control on this is presentation; the engine validates
/// every creation.
pub(crate) fn local_role(state: &AppState, task_id: &str) -> (BoardChatRole, Option<String>) {
    let Some(mut main) = lineage_parent(state, task_id) else {
        return (BoardChatRole::Main, None);
    };
    let mut seen = HashSet::from([task_id.to_string()]);
    while seen.insert(main.clone()) {
        match lineage_parent(state, &main) {
            Some(next) => main = next,
            None => break,
        }
    }
    (BoardChatRole::Side, Some(main))
}

/// Side chats never fork or copy their context into another conversation
/// (chat-model §1): their message-level Fork and context actions are hidden.
pub(crate) fn context_forks_blocked(state: &AppState, task_id: &str) -> bool {
    local_role(state, task_id).0 == BoardChatRole::Side
}

/// Why a side chat has no context fork, wherever one is refused.
pub(crate) const SIDE_FORK_REFUSAL: &str = "Side chats can't fork or carry their context into another chat. Use New side chat for a fresh side chat under the same main chat, or Hand off to a new main chat with a written brief.";

/// What a chat is to the board, as its surfaces need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatBoardPurpose {
    Ordinary,
    Worker,
    Reviewer,
}

/// A chat's role and permitted creations: the engine's identity when it has
/// loaded, else the published lineage with the contract's rules (§6).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChatRoleInfo {
    pub role: BoardChatRole,
    pub main_parent_id: Option<String>,
    pub permitted: ChatPermittedActions,
    pub purpose: ChatBoardPurpose,
    /// From `BoardChatIdentity` (not the local stand-in).
    pub canonical: bool,
    pub parent_unavailable: bool,
}

/// One receipt shown in a chat for a board action taken there.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReceiptKind {
    /// Create ticket from this chat: saved only, no chat was created.
    Created,
    /// Convert this chat to a ticket: this chat is its working chat now.
    Converted,
    /// Hand off to a new main chat: a distinct chat with a written brief.
    HandedOff { task_id: String, started: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChatReceipt {
    pub kind: ReceiptKind,
    pub work_item_id: Option<String>,
    pub key: Option<String>,
    pub title: String,
    pub column: Option<BoardColumn>,
}

/// A Go to chat waiting for its main chat's workbench.
#[derive(Debug, Clone)]
pub(crate) struct PendingReveal {
    pub main: String,
    pub side: String,
    started: Instant,
    asked: Option<Instant>,
    /// How many times the reveal has asked the workbench for the tab.
    asks: u8,
    /// The committed focused chat when it last asked: any other tab (not
    /// the side chat) focused since then is the user's own choice.
    focus_before: Option<String>,
}

/// The status control's open menu.
#[derive(Debug, Clone)]
pub(crate) struct StatusMenu {
    pub task_id: String,
    pub work_item_id: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Per-shell state for ticket identity on chat surfaces.
#[derive(Default)]
pub(crate) struct ChatBoardUi {
    engine_generation: Option<u64>,
    fingerprint: u64,
    /// Working and reviewer badges by chat id.
    badges: HashMap<String, ChatTicketBadge>,
    /// Source/reference tickets by chat id ("Related ticket" receipts).
    related: HashMap<String, Vec<ChatTicketBadge>>,
    /// The engine does not serve badges (older engine, or a slice that has
    /// not landed): nothing is shown until the board changes.
    badges_unavailable: bool,
    fetch: Option<Task<()>>,
    dirty: bool,
    identities: HashMap<String, CanonicalChatIdentity>,
    identity_fetches: HashMap<String, Task<()>>,
    identity_failed: HashSet<String>,
    /// All projects' tickets (palette, Activity, journal-driven refresh).
    model: Option<Entity<BoardModel>>,
    _model_events: Option<Subscription>,
    activity: Vec<WorkItemEvent>,
    activity_loaded: bool,
    activity_fetch: Option<Task<()>>,
    /// Receipts of actions taken in each chat this session.
    receipts: HashMap<String, Vec<ChatReceipt>>,
    reveal: Option<PendingReveal>,
    /// A Go to chat waiting for its chat rows to publish.
    awaiting: Option<(BoardNavigationTarget, Instant)>,
    /// A reveal check is queued for after the current frame.
    reveal_deferred: bool,
    /// The next check of a reveal that is waiting on the engine.
    reveal_timer: Option<Task<()>>,
    nav: Option<Task<()>>,
    op: Option<Task<()>>,
    pub(crate) dialog: Option<dialogs::BoardChatDialog>,
    pub(crate) side_card: Option<dialogs::SideOriginCard>,
    pub(crate) status_menu: Option<StatusMenu>,
    pub(crate) sibling: Option<dialogs::SiblingCreate>,
    #[cfg(feature = "appshots-fixture")]
    pub(crate) probe: Option<fixture::BoardChatProbe>,
}

impl ChatBoardUi {
    /// A dialog, card or menu of this slice holds the keyboard.
    pub(crate) fn owns_keyboard(&self) -> bool {
        self.dialog.is_some() || self.side_card.is_some() || self.status_menu.is_some()
    }

    fn reset_for_engine(&mut self, generation: u64) {
        self.engine_generation = Some(generation);
        self.fingerprint = 0;
        self.badges.clear();
        self.related.clear();
        self.badges_unavailable = false;
        self.fetch = None;
        self.dirty = false;
        self.identities.clear();
        self.identity_fetches.clear();
        self.identity_failed.clear();
        self.activity.clear();
        self.activity_loaded = false;
        self.activity_fetch = None;
        self.status_menu = None;
    }

    pub(crate) fn badge(&self, task_id: &str) -> Option<&ChatTicketBadge> {
        self.badges.get(task_id)
    }

    /// Tickets related to `task_id` that are not its own working ticket.
    pub(crate) fn related(&self, task_id: &str) -> &[ChatTicketBadge] {
        self.related.get(task_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn receipts(&self, task_id: &str) -> &[ChatReceipt] {
        self.receipts.get(task_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn model(&self) -> Option<&Entity<BoardModel>> {
        self.model.as_ref()
    }

    pub(crate) fn activity(&self) -> &[WorkItemEvent] {
        &self.activity
    }

    /// The chat whose working ticket is `work_item_id`, from the badges.
    pub(crate) fn working_chat_of(&self, work_item_id: &str) -> Option<&str> {
        self.badges
            .values()
            .find(|badge| badge.work_item_id == work_item_id && badge.role == TaskLinkRole::Working)
            .map(|badge| badge.task_id.as_str())
    }

    fn push_receipt(&mut self, task_id: &str, receipt: ChatReceipt) {
        let list = self.receipts.entry(task_id.to_string()).or_default();
        list.retain(|existing| {
            existing.work_item_id.is_none() || existing.work_item_id != receipt.work_item_id
        });
        list.push(receipt);
    }

    pub(crate) fn dismiss_receipt(&mut self, task_id: &str, index: usize) {
        if let Some(list) = self.receipts.get_mut(task_id)
            && index < list.len()
        {
            list.remove(index);
        }
    }

    fn install(&mut self, badges: Vec<ChatTicketBadge>, related: Vec<ChatTicketBadge>) {
        let mut by_chat: HashMap<String, ChatTicketBadge> = HashMap::new();
        let mut extra = Vec::new();
        for badge in badges {
            match badge.role {
                // Only a working or reviewer link is identity; anything else
                // the engine lists is a relationship, shown as a receipt.
                TaskLinkRole::Working | TaskLinkRole::Reviewer => {
                    let replace = by_chat.get(&badge.task_id).is_none_or(|current| {
                        current.role != TaskLinkRole::Working && badge.role == TaskLinkRole::Working
                    });
                    if replace {
                        by_chat.insert(badge.task_id.clone(), badge);
                    }
                }
                _ => extra.push(badge),
            }
        }
        let mut related_by_chat: HashMap<String, Vec<ChatTicketBadge>> = HashMap::new();
        for badge in related.into_iter().chain(extra) {
            // A chat's own working or reviewer ticket is never also
            // "related" to it.
            if by_chat
                .get(&badge.task_id)
                .is_some_and(|own| own.work_item_id == badge.work_item_id)
            {
                continue;
            }
            let list = related_by_chat.entry(badge.task_id.clone()).or_default();
            if !list
                .iter()
                .any(|existing| existing.work_item_id == badge.work_item_id)
            {
                list.push(badge);
            }
        }
        self.badges = by_chat;
        self.related = related_by_chat;
        self.badges_unavailable = false;
    }
}

impl Shell {
    /// A measuring child that records its parent's painted bounds under
    /// `label` for the A10 QA probe; `None` (nothing drawn) outside it. The
    /// parent must be `relative()`.
    pub(crate) fn board_probe_child(
        &self,
        label: impl FnOnce() -> String,
    ) -> Option<gpui::AnyElement> {
        #[cfg(feature = "appshots-fixture")]
        if let Some(probe) = &self.board_chat.probe {
            use gpui::{IntoElement as _, Styled as _};
            let bounds = probe.bounds.clone();
            let label = label();
            return Some(
                gpui::canvas(
                    move |painted, _, _| {
                        bounds.borrow_mut().insert(label.clone(), painted);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .into_any_element(),
            );
        }
        #[cfg(not(feature = "appshots-fixture"))]
        let _ = label;
        None
    }

    /// Record what a board surface drew (its words) for the QA probe.
    pub(crate) fn board_mark(
        &self,
        key: impl FnOnce() -> String,
        values: impl FnOnce() -> Vec<String>,
    ) {
        #[cfg(feature = "appshots-fixture")]
        if let Some(probe) = &self.board_chat.probe {
            probe.marks.borrow_mut().insert(key(), values());
        }
        #[cfg(not(feature = "appshots-fixture"))]
        let _ = (key, values);
    }

    pub(crate) fn board_chat_api(&self, cx: &App) -> Option<BoardApi> {
        self.state.read(cx).engine().cloned().map(BoardApi::new)
    }

    /// From `on_state_changed`: follow the engine connection and the chat
    /// set (a cheap fingerprint), and settle a pending Go to chat.
    pub(in crate::shell) fn board_chat_state_changed(&mut self, cx: &mut Context<Self>) {
        let (generation, has_engine, fingerprint) = {
            let state = self.state.read(cx);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for chat in &state.chats {
                chat.id.hash(&mut hasher);
                chat.archived.hash(&mut hasher);
            }
            (
                state.engine_generation(),
                state.engine().is_some(),
                hasher.finish(),
            )
        };
        if self.board_chat.engine_generation != Some(generation) {
            self.board_chat.reset_for_engine(generation);
        }
        if has_engine && self.board_chat.model.is_none() {
            let state = self.state.clone();
            let model = cx.new(|cx| BoardModel::new(state, BoardScope::AllProjects, cx));
            let events = cx.subscribe(&model, |shell, _, event: &BoardModelEvent, cx| {
                shell.board_chat_model_event(event, cx)
            });
            self.board_chat.model = Some(model);
            self.board_chat._model_events = Some(events);
        }
        if has_engine && fingerprint != self.board_chat.fingerprint {
            self.board_chat.fingerprint = fingerprint;
            self.board_chat_refresh(cx);
        }
        self.board_chat_settle_reveal(cx);
    }

    fn board_chat_model_event(&mut self, event: &BoardModelEvent, cx: &mut Context<Self>) {
        match event {
            BoardModelEvent::Events(events) => {
                // Any committed board change can add, move or rename a badge:
                // re-read them, and the identities (purpose) lazily.
                self.board_chat.identities.clear();
                self.board_chat.identity_failed.clear();
                self.board_chat_refresh(cx);
                let mut fresh: Vec<WorkItemEvent> = events
                    .iter()
                    .filter(|event| event.activity)
                    .cloned()
                    .collect();
                fresh.reverse();
                let activity = &mut self.board_chat.activity;
                activity.retain(|known| !fresh.iter().any(|event| event.id == known.id));
                fresh.append(activity);
                fresh.truncate(ACTIVITY_KEPT);
                *activity = fresh;
            }
            BoardModelEvent::Changed => {
                if !self.board_chat.activity_loaded {
                    self.board_chat_load_activity(cx);
                }
            }
        }
        cx.notify();
    }

    /// The recent activity journal for the Activity view (newest first).
    fn board_chat_load_activity(&mut self, cx: &mut Context<Self>) {
        if self.board_chat.activity_fetch.is_some() {
            return;
        }
        let Some(model) = self.board_chat.model.clone() else {
            return;
        };
        if !matches!(model.read(cx).status(), super::model::BoardStatus::Live) {
            return;
        }
        let Some(api) = self.board_chat_api(cx) else {
            return;
        };
        let head = model.read(cx).as_of_seq();
        let generation = self.state.read(cx).engine_generation();
        let request = BoardEventsRequest {
            scope: BoardScope::AllProjects,
            work_item_id: None,
            after_seq: Some(head.saturating_sub(400)),
            limit: Some(200),
            activity_only: true,
        };
        self.board_chat.activity_fetch = Some(cx.spawn(async move |this, cx| {
            let result = api.events(request).await;
            this.update(cx, |shell, cx| {
                shell.board_chat.activity_fetch = None;
                if shell.board_chat.engine_generation != Some(generation) {
                    return;
                }
                shell.board_chat.activity_loaded = true;
                if let Ok(page) = result {
                    let mut events = page.events;
                    events.reverse();
                    let known = std::mem::take(&mut shell.board_chat.activity);
                    let mut merged = known;
                    for event in events {
                        if !merged.iter().any(|existing| existing.id == event.id) {
                            merged.push(event);
                        }
                    }
                    merged.sort_by_key(|event| std::cmp::Reverse(event.seq));
                    merged.truncate(ACTIVITY_KEPT);
                    shell.board_chat.activity = merged;
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Re-read the badges of every chat (coalesced while a read runs).
    pub(crate) fn board_chat_refresh(&mut self, cx: &mut Context<Self>) {
        if self.board_chat.fetch.is_some() {
            self.board_chat.dirty = true;
            return;
        }
        let Some(api) = self.board_chat_api(cx) else {
            return;
        };
        let generation = self.state.read(cx).engine_generation();
        let ids: Vec<String> = self
            .state
            .read(cx)
            .chats
            .iter()
            .map(|chat| chat.id.clone())
            .collect();
        self.board_chat.fetch = Some(cx.spawn(async move |this, cx| {
            let mut badges = Vec::new();
            let mut related = Vec::new();
            let mut failure = None;
            for chunk in ids.chunks(BADGE_CHUNK) {
                match api
                    .chat_badges(BoardChatBadgesRequest {
                        task_ids: chunk.to_vec(),
                    })
                    .await
                {
                    Ok(page) => {
                        badges.extend(page.badges);
                        related.extend(page.related);
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            this.update(cx, |shell, cx| {
                shell.board_chat_install(generation, badges, related, failure, cx)
            })
            .ok();
        }));
    }

    fn board_chat_install(
        &mut self,
        generation: u64,
        badges: Vec<ChatTicketBadge>,
        related: Vec<ChatTicketBadge>,
        failure: Option<BoardCallError>,
        cx: &mut Context<Self>,
    ) {
        self.board_chat.fetch = None;
        if self.board_chat.engine_generation != Some(generation) {
            self.board_chat_refresh(cx);
            return;
        }
        match failure {
            None => self.board_chat.install(badges, related),
            Some(error) if board_unavailable(&error) => {
                self.board_chat.badges.clear();
                self.board_chat.related.clear();
                self.board_chat.badges_unavailable = true;
            }
            // A transient failure keeps what was shown; the next board or
            // chat change reads again.
            Some(error) => tracing::debug!(error = %error.message(), "board badges read failed"),
        }
        if std::mem::take(&mut self.board_chat.dirty) {
            self.board_chat_refresh(cx);
        }
        cx.notify();
    }

    /// Load `task_id`'s canonical identity once (menus and creation cards
    /// ask for it when they open).
    pub(crate) fn board_chat_ensure_identity(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let ui = &self.board_chat;
        if task_id.is_empty()
            || ui.identities.contains_key(task_id)
            || ui.identity_fetches.contains_key(task_id)
            || ui.identity_failed.contains(task_id)
        {
            return;
        }
        let Some(api) = self.board_chat_api(cx) else {
            return;
        };
        let generation = self.state.read(cx).engine_generation();
        let id = task_id.to_string();
        let task = cx.spawn(async move |this, cx| {
            let result = api
                .chat_identity(BoardChatIdentityRequest {
                    task_id: id.clone(),
                })
                .await;
            this.update(cx, |shell, cx| {
                shell.board_chat.identity_fetches.remove(&id);
                if shell.board_chat.engine_generation != Some(generation) {
                    return;
                }
                match result {
                    Ok(identity) => {
                        shell.board_chat.identities.insert(id, identity);
                    }
                    Err(_) => {
                        shell.board_chat.identity_failed.insert(id);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.board_chat
            .identity_fetches
            .insert(task_id.to_string(), task);
    }

    /// `task_id`'s role and what it may create (contract §6).
    pub(crate) fn board_chat_role(&self, task_id: &str, cx: &App) -> ChatRoleInfo {
        let badge = self.board_chat.badge(task_id);
        let badge_purpose = match badge.map(|badge| badge.role) {
            Some(TaskLinkRole::Working) => ChatBoardPurpose::Worker,
            Some(TaskLinkRole::Reviewer) => ChatBoardPurpose::Reviewer,
            _ => ChatBoardPurpose::Ordinary,
        };
        if let Some(identity) = self.board_chat.identities.get(task_id) {
            let purpose = match identity.purpose {
                ChatPurpose::Ordinary => badge_purpose,
                ChatPurpose::TicketWorker { .. } => ChatBoardPurpose::Worker,
                ChatPurpose::Reviewer { .. } => ChatBoardPurpose::Reviewer,
            };
            return ChatRoleInfo {
                role: identity.role,
                main_parent_id: identity.main_parent_id.clone(),
                permitted: identity.permitted.clone(),
                purpose,
                canonical: true,
                parent_unavailable: identity.parent_unavailable,
            };
        }
        let state = self.state.read(cx);
        let (role, main_parent_id) = local_role(state, task_id);
        let convertible = badge_purpose == ChatBoardPurpose::Ordinary;
        let permitted = match role {
            BoardChatRole::Main => ChatPermittedActions {
                create_side_fresh: true,
                create_side_with_context: true,
                create_main: true,
                convert_to_ticket: convertible,
                start_ticket: true,
                ..Default::default()
            },
            BoardChatRole::Side => ChatPermittedActions {
                create_sibling_fresh: true,
                create_main_with_handoff: true,
                convert_to_ticket: convertible,
                start_ticket: true,
                ..Default::default()
            },
        };
        let parent_unavailable = main_parent_id
            .as_deref()
            .is_some_and(|main| !state.chats.iter().any(|chat| chat.id == main));
        ChatRoleInfo {
            role,
            main_parent_id,
            permitted,
            purpose: badge_purpose,
            canonical: false,
            parent_unavailable,
        }
    }

    // ---- Go to chat ----

    /// Go to chat: the one entry point for Board cards, the detail drawer,
    /// receipts, Activity, search and the palette. Resolves the ticket's or
    /// chat's canonical location through the engine (`BoardNavigation`) and
    /// reveals it ([`Self::board_open_navigation`]). Never relaunches,
    /// resends, promotes or reparents. Without the engine operation (an
    /// older engine) the published lineage resolves a known chat instead.
    pub(crate) fn board_go_to_chat(
        &mut self,
        request: BoardNavigationRequest,
        cx: &mut Context<Self>,
    ) {
        let fallback = self.board_local_navigation(&request, cx);
        let Some(api) = self.board_chat_api(cx) else {
            match fallback {
                Some(target) => self.board_open_navigation(target, cx),
                None => self.board_chat_notice_error("Engine not connected.", cx),
            }
            return;
        };
        self.board_chat.nav = Some(cx.spawn(async move |this, cx| {
            let result = api.navigation(request).await;
            this.update(cx, |shell, cx| {
                shell.board_chat.nav = None;
                match result {
                    Ok(target) => shell.board_open_navigation(target, cx),
                    Err(error) if board_unavailable(&error) => match fallback {
                        Some(target) => shell.board_open_navigation(target, cx),
                        None => shell.board_chat_notice_error(error.message(), cx),
                    },
                    Err(error) => shell.board_chat_notice_error(error.message(), cx),
                }
            })
            .ok();
        }));
    }

    /// The location of a chat or ticket from what this window already knows
    /// (badges, the Board list, published lineage).
    fn board_local_navigation(
        &self,
        request: &BoardNavigationRequest,
        cx: &App,
    ) -> Option<BoardNavigationTarget> {
        let task_id = request.task_id.clone().or_else(|| {
            let work_item_id = request.work_item_id.as_deref()?;
            self.board_chat
                .working_chat_of(work_item_id)
                .map(str::to_string)
                .or_else(|| {
                    self.board_chat
                        .model
                        .as_ref()?
                        .read(cx)
                        .item(work_item_id)?
                        .working_chat
                        .as_ref()
                        .map(|chat| chat.task_id.clone())
                })
        });
        let Some(task_id) = task_id else {
            return request
                .work_item_id
                .clone()
                .map(|work_item_id| BoardNavigationTarget::NoChat { work_item_id });
        };
        let state = self.state.read(cx);
        if !state.chats.iter().any(|chat| chat.id == task_id) {
            return None;
        }
        Some(match local_role(state, &task_id) {
            (BoardChatRole::Main, _) => BoardNavigationTarget::MainChat { task_id },
            (BoardChatRole::Side, Some(main))
                if state
                    .chats
                    .iter()
                    .any(|chat| chat.id == main && !chat.archived) =>
            {
                BoardNavigationTarget::SideChat {
                    main_task_id: main,
                    side_task_id: task_id,
                }
            }
            (BoardChatRole::Side, main) => BoardNavigationTarget::Direct {
                task_id,
                original_parent_id: main,
            },
        })
    }

    /// Reveal a resolved location (receipts carry one already): open the
    /// main chat, or its workbench with the exact side chat selected (its
    /// tab reopened when it was closed). A view only: nothing is launched,
    /// sent, promoted or reparented.
    pub(crate) fn board_open_navigation(
        &mut self,
        target: BoardNavigationTarget,
        cx: &mut Context<Self>,
    ) {
        // A chat the engine has just created (a sibling, a handoff) may not
        // have published its row here yet: wait for it, bounded.
        let awaited: Vec<&str> = match &target {
            BoardNavigationTarget::MainChat { task_id }
            | BoardNavigationTarget::Direct { task_id, .. } => vec![task_id.as_str()],
            BoardNavigationTarget::SideChat {
                main_task_id,
                side_task_id,
            } => vec![main_task_id.as_str(), side_task_id.as_str()],
            BoardNavigationTarget::NoChat { .. } => Vec::new(),
        };
        let state = self.state.read(cx);
        if awaited
            .iter()
            .any(|id| !state.chats.iter().any(|chat| chat.id == *id))
        {
            self.board_chat.awaiting = Some((target, Instant::now()));
            return;
        }
        self.board_open_navigation_now(target, cx);
    }

    /// Open a target whose chats are published (or whose wait ran out).
    fn board_open_navigation_now(&mut self, target: BoardNavigationTarget, cx: &mut Context<Self>) {
        self.board_chat.awaiting = None;
        self.board_chat.reveal = None;
        let known = |shell: &Self, id: &str, cx: &App| {
            shell.state.read(cx).chats.iter().any(|chat| chat.id == id)
        };
        match target {
            BoardNavigationTarget::MainChat { task_id } => {
                if known(self, &task_id, cx) {
                    self.open_chat(task_id, cx);
                } else {
                    self.board_chat_notice_error(
                        "That chat is not available on this device yet.",
                        cx,
                    );
                }
            }
            BoardNavigationTarget::SideChat {
                main_task_id,
                side_task_id,
            } => {
                if !known(self, &side_task_id, cx) {
                    self.board_chat_notice_error(
                        "That side chat is not available on this device yet.",
                        cx,
                    );
                    return;
                }
                if !known(self, &main_task_id, cx) {
                    // The main chat is gone: open the side chat on its own;
                    // its recorded parent stays as it is.
                    self.open_chat(side_task_id, cx);
                    return;
                }
                if self.state.read(cx).selected_chat.as_deref() != Some(main_task_id.as_str())
                    || !matches!(self.route, crate::shell::Route::Chat)
                {
                    self.open_chat(main_task_id.clone(), cx);
                }
                self.board_chat.reveal = Some(PendingReveal {
                    main: main_task_id,
                    side: side_task_id,
                    started: Instant::now(),
                    asked: None,
                    asks: 0,
                    focus_before: None,
                });
                self.board_chat_settle_reveal(cx);
            }
            BoardNavigationTarget::Direct { task_id, .. } => {
                if known(self, &task_id, cx) {
                    self.open_chat(task_id, cx);
                    self.sidebar_notice = Some(SidebarNotice::transient_status(
                        "Its main chat is unavailable, so this side chat opened on its own. Its parent is unchanged.",
                    ));
                } else {
                    self.board_chat_notice_error(
                        "That chat is not available on this device yet.",
                        cx,
                    );
                }
            }
            BoardNavigationTarget::NoChat { work_item_id } => {
                self.chat_open_ticket(work_item_id, cx);
                self.sidebar_notice = Some(SidebarNotice::transient_status(
                    "This ticket has no working chat yet. Move it to Ready to start it.",
                ));
            }
        }
        cx.notify();
    }

    /// Finish a pending Go to chat once the main chat's workbench is loaded:
    /// open (or reopen) the side chat's tab there and select it. Called on
    /// state changes and while the workbench renders.
    pub(in crate::shell) fn board_chat_settle_reveal(&mut self, cx: &mut Context<Self>) {
        if let Some((target, started)) = self.board_chat.awaiting.clone() {
            let published = {
                let state = self.state.read(cx);
                let has = |id: &str| state.chats.iter().any(|chat| chat.id == id);
                match &target {
                    BoardNavigationTarget::MainChat { task_id }
                    | BoardNavigationTarget::Direct { task_id, .. } => has(task_id),
                    BoardNavigationTarget::SideChat {
                        main_task_id,
                        side_task_id,
                    } => has(main_task_id) && has(side_task_id),
                    BoardNavigationTarget::NoChat { .. } => true,
                }
            };
            if published || started.elapsed() > REVEAL_TIMEOUT {
                self.board_open_navigation_now(target, cx);
            }
            return;
        }
        let Some(reveal) = self.board_chat.reveal.clone() else {
            return;
        };
        let selected = self.state.read(cx).selected_chat.clone();
        if selected.as_deref() != Some(reveal.main.as_str())
            || !matches!(self.route, crate::shell::Route::Chat)
        {
            // The user went elsewhere (the main chat is selected before the
            // reveal starts): it no longer applies and never steals focus.
            self.board_chat.reveal = None;
            self.board_chat.reveal_timer = None;
            return;
        }
        if !self.workbench.capable() {
            // No workbench on this engine: the side chat opens as the chat.
            self.board_chat.reveal = None;
            self.open_side_chat_in_pane(reveal.side, cx);
            return;
        }
        // Decide on the engine's committed layout only: an optimistic open
        // the engine then refuses (its layout changed meanwhile) must not
        // count as revealed.
        let ready = self.workbench.main() == reveal.main
            && self.workbench.loaded()
            && self.workbench.settled();
        let committed_focus = self
            .workbench
            .committed()
            .and_then(|layout| layout.focused_chat_id().map(str::to_string));
        let (focused, placed) = self
            .workbench
            .committed()
            .map(|layout| {
                (
                    layout.focused_chat_id() == Some(reveal.side.as_str()),
                    layout.contains_chat(&reveal.side)
                        || layout
                            .closed_tabs
                            .iter()
                            .any(|closed| closed.tab.chat_id == reveal.side),
                )
            })
            .unwrap_or((false, false));
        if ready && focused {
            self.board_chat.reveal = None;
            self.board_chat.reveal_timer = None;
            return;
        }
        // After it asked, a different tab focused (neither the side chat nor
        // what was focused when it asked) is the user's choice: stop. So is
        // a third refusal; the side chat stays one click away in Related.
        if ready
            && reveal.asks > 0
            && (committed_focus != reveal.focus_before || reveal.asks >= REVEAL_ASKS)
        {
            self.board_chat.reveal = None;
            self.board_chat.reveal_timer = None;
            return;
        }
        if reveal.started.elapsed() > REVEAL_TIMEOUT {
            self.board_chat.reveal = None;
            self.board_chat.reveal_timer = None;
            self.board_chat_notice_error(
                "The main chat's workbench did not load. Open the side chat from its Related chats.",
                cx,
            );
            return;
        }
        // A chat the engine has just created is placed in its main chat's
        // workbench by the engine itself: wait briefly for that placement
        // rather than racing it with a second open of the same tab.
        let awaiting_placement = !placed && reveal.started.elapsed() < PLACEMENT_WAIT;
        if !ready
            || awaiting_placement
            || reveal
                .asked
                .is_some_and(|asked| asked.elapsed() < REVEAL_RETRY)
        {
            self.board_chat_schedule_reveal(cx);
            return;
        }
        if let Some(pending) = self.board_chat.reveal.as_mut() {
            pending.asked = Some(Instant::now());
            pending.asks += 1;
            pending.focus_before = committed_focus;
        }
        self.open_side_chat_in_pane(reveal.side, cx);
        self.board_chat_schedule_reveal(cx);
    }

    /// Check a pending reveal again shortly: nothing else may redraw while
    /// the workbench waits for the engine.
    fn board_chat_schedule_reveal(&mut self, cx: &mut Context<Self>) {
        if self.board_chat.reveal_timer.is_some() {
            return;
        }
        self.board_chat.reveal_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REVEAL_POLL).await;
            this.update(cx, |shell, cx| {
                shell.board_chat.reveal_timer = None;
                shell.board_chat_settle_reveal(cx);
            })
            .ok();
        }));
    }

    /// Open a ticket's detail: the Board page with the ticket selected and
    /// its detail drawer open (the page owns its selection and saves it).
    pub(crate) fn chat_open_ticket(&mut self, work_item_id: String, cx: &mut Context<Self>) {
        self.board_chat.status_menu = None;
        let page = self.board_page(cx);
        self.open_sidebar_page(SidebarPage::Board, cx);
        page.update(cx, |page, cx| page.show_ticket(work_item_id, cx));
    }

    pub(crate) fn board_chat_notice_error(
        &mut self,
        message: impl Into<gpui::SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_notice = Some(SidebarNotice::transient_error(message));
        cx.notify();
    }

    // ---- ticket operations from a chat ----

    /// The status control's moves: Start/Continue (Ready) or Stop/Reopen
    /// (Backlog). Done is engine-owned and never offered.
    pub(crate) fn board_move_ticket(
        &mut self,
        work_item_id: String,
        to: BoardColumn,
        cx: &mut Context<Self>,
    ) {
        self.board_chat.status_menu = None;
        if to == BoardColumn::Done {
            return;
        }
        let Some(api) = self.board_chat_api(cx) else {
            self.board_chat_notice_error("Engine not connected.", cx);
            return;
        };
        let request = BoardMoveRequest {
            request_id: new_request_id("chat-move"),
            work_item_id,
            to,
            expected_revision: None,
            after_id: None,
            before_id: None,
            destination: BoardDestination::Auto,
            context: BoardContextMode::Fresh,
            execution: None,
            follow_up: BoardFollowUp::None,
        };
        self.board_chat.op = Some(cx.spawn(async move |this, cx| {
            let result = api.move_item(request).await;
            this.update(cx, |shell, cx| {
                shell.board_chat.op = None;
                match result {
                    Ok(receipt) => {
                        if let Some(model) = shell.board_chat.model.clone() {
                            model
                                .update(cx, |model, cx| model.apply_receipt(&receipt.mutation, cx));
                        }
                        shell.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                            "{} is in {} · {}.",
                            receipt.mutation.key,
                            receipt.mutation.column.label(),
                            receipt.phase.label()
                        )));
                        shell.board_chat_refresh(cx);
                    }
                    Err(error) => shell.board_chat_notice_error(error.message(), cx),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Rename a ticket's working chat from its header: one engine operation
    /// on the ticket renames both (contract §7.6, title sync). Returns false
    /// for a chat that is not a working chat (the ordinary rename applies).
    pub(in crate::shell) fn board_rename_working_chat(
        &mut self,
        chat_id: &str,
        title: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(badge) = self
            .board_chat
            .badge(chat_id)
            .filter(|badge| badge.role == TaskLinkRole::Working)
            .cloned()
        else {
            return false;
        };
        let Some(api) = self.board_chat_api(cx) else {
            return false;
        };
        let title = title.trim().to_string();
        if title.is_empty() {
            return true;
        }
        self.board_chat.op = Some(cx.spawn(async move |this, cx| {
            let mut outcome = Err(BoardCallError::Other(
                "The rename was not attempted.".into(),
            ));
            // A concurrent edit of another field merges; only a title edit
            // racing ours asks again on the current record (once).
            for _ in 0..2 {
                let detail = match api
                    .read(BoardReadRequest {
                        work_item_id: badge.work_item_id.clone(),
                        project_id: None,
                        history_limit: Some(1),
                    })
                    .await
                {
                    Ok(detail) => detail,
                    Err(error) => {
                        outcome = Err(error);
                        break;
                    }
                };
                outcome = api
                    .update(BoardUpdateRequest {
                        request_id: new_request_id("chat-rename"),
                        work_item_id: badge.work_item_id.clone(),
                        expected_revision: detail.view.item.revision,
                        changes: BoardItemChanges {
                            title: Some(title.clone()),
                            ..Default::default()
                        },
                    })
                    .await;
                if !matches!(
                    &outcome,
                    Err(BoardCallError::Board(BoardError::StaleRevision { .. }))
                ) {
                    break;
                }
            }
            this.update(cx, |shell, cx| {
                shell.board_chat.op = None;
                match outcome {
                    Ok(receipt) => {
                        if let Some(model) = shell.board_chat.model.clone() {
                            model.update(cx, |model, cx| model.apply_receipt(&receipt, cx));
                        }
                        shell.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                            "Renamed {} and its working chat.",
                            receipt.key
                        )));
                        shell.board_chat_refresh(cx);
                    }
                    Err(error) => shell.board_chat_notice_error(error.message(), cx),
                }
                cx.notify();
            })
            .ok();
        }));
        true
    }
}
