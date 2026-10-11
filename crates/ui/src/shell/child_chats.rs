//! Child chats: status, counts and the Related place's entry point.
//!
//! Persistent child chats (`Chat.child_thread`) belong to the conversation
//! that started them. Instead of an indented sidebar tree, the parent's
//! Related place lists them with its side chats ([`RightPlace::Related`],
//! [`super::related_chats`]). A child opens in the canvas like any related
//! chat, never inside the sidebar.

use super::*;
use chrono::DateTime;
use luna_proto::{Chat, ChatIndicator};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildChatStatus {
    NeedsApproval,
    NeedsInput,
    Running,
    Failed,
    Interrupted,
    Completed,
    Idle,
    Archived,
}

impl ChildChatStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NeedsApproval => "Needs approval",
            Self::NeedsInput => "Needs input",
            Self::Running => "Running",
            Self::Failed => "Failed",
            Self::Interrupted => "Stopped",
            // A child's own turn ended; its owned background work (if any)
            // is the work view's, never implied done here.
            Self::Completed => "Turn completed",
            Self::Idle => "Idle",
            Self::Archived => "Archived",
        }
    }

    /// Readback order: attention, then live work, then everything else by
    /// recency; archived rows last.
    #[cfg(feature = "appshots-fixture")]
    fn rank(self) -> u8 {
        match self {
            Self::NeedsApproval | Self::NeedsInput => 0,
            Self::Running => 1,
            Self::Archived => 3,
            _ => 2,
        }
    }

    /// The sidebar's status palette, so a child reads the same in both places.
    pub(crate) fn color(self, theme: &Theme) -> gpui::Hsla {
        let indicator = match self {
            Self::NeedsApproval => ChatIndicator::AwaitingApproval,
            Self::NeedsInput => ChatIndicator::AwaitingInput,
            Self::Running => ChatIndicator::Working,
            Self::Failed => ChatIndicator::Errored,
            Self::Completed => ChatIndicator::Completed,
            Self::Interrupted | Self::Idle | Self::Archived => {
                return theme.readable_muted(0.55);
            }
        };
        let color = spaces::status_dot_color(indicator, theme);
        // The sidebar dims status words; a list row is roomier and keeps
        // them legible.
        gpui::Hsla {
            a: color.a.max(0.85),
            ..color
        }
    }
}

#[cfg(feature = "appshots-fixture")]
pub(super) struct ChildChatRow {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: ChildChatStatus,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) detail: Option<String>,
    pub(super) model: Option<String>,
    pub(super) children: usize,
}

#[cfg(feature = "appshots-fixture")]
fn is_child_of(chat: &Chat, parent: &str) -> bool {
    chat.id != parent
        && chat
            .child_thread
            .as_ref()
            .is_some_and(|child| child.parent_task_id == parent)
}

/// Non-archived direct child chats per parent id (QA readback).
#[cfg(feature = "appshots-fixture")]
pub(super) fn direct_child_counts(chats: &[Chat]) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    for chat in chats.iter().filter(|chat| !chat.archived) {
        if let Some(child) = &chat.child_thread
            && child.parent_task_id != chat.id
        {
            *counts.entry(child.parent_task_id.clone()).or_default() += 1;
        }
    }
    counts
}

/// Live session state wins; otherwise the newest recorded child outcome, then
/// the seen marker. Outcomes are compacted after delivery, so a reported child
/// can read Idle.
pub(crate) fn child_chat_status(
    state: &AppState,
    chat: &Chat,
    now: DateTime<Utc>,
) -> ChildChatStatus {
    if chat.archived {
        return ChildChatStatus::Archived;
    }
    if state.send_pending(&chat.id, now) {
        return ChildChatStatus::Running;
    }
    match luna_proto::view::effective_indicator(state.session_for(&chat.id), now) {
        Indicator::Working => return ChildChatStatus::Running,
        Indicator::AwaitingApproval => return ChildChatStatus::NeedsApproval,
        Indicator::AwaitingInput => return ChildChatStatus::NeedsInput,
        Indicator::Errored => return ChildChatStatus::Failed,
        Indicator::None => {}
    }
    let latest = chat.child_thread.as_ref().and_then(|child| {
        child
            .outcomes
            .iter()
            .max_by_key(|outcome| outcome.created_at_ms)
    });
    match latest.map(|outcome| outcome.outcome.as_str()) {
        Some("failed" | "errored") => ChildChatStatus::Failed,
        Some("interrupted") => ChildChatStatus::Interrupted,
        Some("turn-completed") => ChildChatStatus::Completed,
        // Unread alone never implies a completed turn.
        _ => ChildChatStatus::Idle,
    }
}

pub(crate) fn provider_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Codex => "Codex",
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Mock => "Mock",
        _ => "Provider",
    }
}

#[cfg(feature = "appshots-fixture")]
fn first_line(text: &str) -> Option<String> {
    let line = transcript::single_line(text);
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// This parent's direct children: attention and running first, then the rest
/// by recency, archived last.
#[cfg(feature = "appshots-fixture")]
pub(super) fn child_chat_rows(
    state: &AppState,
    parent: &str,
    now: DateTime<Utc>,
) -> Vec<ChildChatRow> {
    let counts = direct_child_counts(&state.chats);
    let mut rows: Vec<ChildChatRow> = state
        .chats
        .iter()
        .filter(|chat| is_child_of(chat, parent))
        .map(|chat| {
            let child = chat.child_thread.as_ref();
            let detail = child
                .and_then(|child| {
                    child
                        .outcomes
                        .iter()
                        .max_by_key(|outcome| outcome.created_at_ms)
                        .and_then(|outcome| first_line(&outcome.summary))
                        .or_else(|| first_line(&child.initial_prompt))
                })
                .filter(|line| Some(line.as_str()) != chat.title.as_deref());
            let model = chat.config.as_ref().map(|config| {
                let provider = provider_label(config.harness);
                match config.model.as_deref().filter(|model| !model.is_empty()) {
                    Some(model) => format!("{provider} · {model}"),
                    None => provider.to_string(),
                }
            });
            ChildChatRow {
                id: chat.id.clone(),
                title: transcript::single_line(chat.title.as_deref().unwrap_or("Side chat")),
                status: child_chat_status(state, chat, now),
                updated_at: chat.last_message_at.unwrap_or(chat.created_at),
                detail,
                model,
                children: counts.get(&chat.id).copied().unwrap_or(0),
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        left.status
            .rank()
            .cmp(&right.status.rank())
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

impl Shell {
    /// Promote a pane chat to the main view. The pane composer's draft is
    /// made durable before the main composer loads it, so the text moves
    /// with the chat instead of two composers editing one draft.
    pub(super) fn open_pane_chat_as_main(&mut self, id: String, cx: &mut Context<Self>) {
        self.pending_open_main = None;
        let Some(composer) = self.pane_composers.read(cx).get(&id) else {
            self.open_chat(id, cx);
            return;
        };
        if composer.update(cx, |composer, cx| composer.prepare_close(cx)) {
            self.finish_open_pane_chat_as_main(id, cx);
            return;
        }
        let owner_chat = self.active_chat.clone();
        let owner_generation = self.state.read(cx).engine_generation();
        self.pending_open_main = Some(cx.subscribe(&composer, move |shell, _, event, cx| {
            if matches!(event, ComposerEvent::DraftFlushCompleted) {
                shell.pending_open_main = None;
                if shell.active_chat != owner_chat
                    || shell.state.read(cx).engine_generation() != owner_generation
                {
                    return;
                }
                shell.finish_open_pane_chat_as_main(id.clone(), cx);
            }
        }));
    }

    fn finish_open_pane_chat_as_main(&mut self, id: String, cx: &mut Context<Self>) {
        // A composer with a send or receipt check in flight keeps owning it;
        // only an idle one is dropped so the main view reloads the draft.
        let idle = self
            .pane_composers
            .read(cx)
            .get(&id)
            .is_some_and(|composer| !composer.read(cx).has_work_in_flight());
        if idle {
            self.pane_composers
                .update(cx, |composers, _| composers.remove(&id));
        }
        self.composer
            .update(cx, |composer, _| composer.forget_cached_draft(&id));
        self.open_chat(id, cx);
    }

    /// The parent link: bring the parent conversation forward in the main view.
    pub(super) fn show_pane_parent(&mut self, parent: String, cx: &mut Context<Self>) {
        if self.right_pane_expanded {
            self.toggle_right_pane_expand(cx);
        }
        if self.active_chat != parent {
            self.open_chat(parent, cx);
        }
        cx.notify();
    }

    /// The current chat's non-archived child count (QA readback).
    #[cfg(feature = "appshots-fixture")]
    pub(super) fn active_child_chat_count(&self, cx: &App) -> (usize, bool) {
        if self.active_chat.is_empty() {
            return (0, false);
        }
        let chats = &self.state.read(cx).chats;
        let mut active = 0;
        let mut any = false;
        for chat in chats
            .iter()
            .filter(|chat| is_child_of(chat, &self.active_chat))
        {
            any = true;
            if !chat.archived {
                active += 1;
            }
        }
        (active, any)
    }

    /// Open `parent`'s Related place, selecting the parent first when
    /// needed (sidebar count chip). Pre-seeding the parent's panel state
    /// lets the chat switch restore straight onto it.
    pub(super) fn open_related_chats_for(&mut self, parent: String, cx: &mut Context<Self>) {
        if parent == self.active_chat {
            self.show_right_place(RightPlace::Related, cx);
            return;
        }
        self.panels.update(&parent, |panel| {
            panel.changes_open = true;
            panel.right_active = RightSurface::Home(RightPlace::Related);
        });
        self.open_chat(parent, cx);
    }
}

/// Native visual QA drives the production panel through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_right_pane(&mut self, cx: &mut Context<Self>) {
        if !self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
        let key = self.panel_key(cx);
        self.panels.update(&key, |panel| {
            panel.right_active = RightSurface::Home(RightPlace::Overview)
        });
        cx.notify();
    }

    pub fn fixture_open_child_chats(&mut self, cx: &mut Context<Self>) {
        self.show_right_place(RightPlace::Related, cx);
    }

    pub fn fixture_open_child_chats_for(&mut self, parent: String, cx: &mut Context<Self>) {
        self.open_related_chats_for(parent, cx);
    }

    pub fn fixture_main_composer_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        self.composer.read(cx).readback(cx)
    }

    pub fn fixture_set_right_pane_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.settings.right_pane_width = width;
        cx.notify();
    }

    pub fn fixture_engine(&self, cx: &Context<Self>) -> Option<crate::state::EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    pub fn fixture_close_right_pane(&mut self, cx: &mut Context<Self>) {
        if self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
    }

    pub fn fixture_child_chats_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let state = self.state.read(cx);
        let now = Utc::now();
        let (badge, any) = self.active_child_chat_count(cx);
        serde_json::json!({
            "activeChat": self.active_chat,
            "rightPaneOpen": self.right_pane_open(cx),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "relatedBadge": badge,
            "relatedAny": any,
            "mainComposer": self.composer.read(cx).readback(cx),
            "paneComposerStates": self
                .pane_composers
                .read(cx)
                .ids()
                .into_iter()
                .filter_map(|id| {
                    let composer = self.pane_composers.read(cx).get(&id)?;
                    Some((id, composer.read(cx).readback(cx)))
                })
                .collect::<serde_json::Map<_, _>>(),
            "paneChats": state.pane_chat_ids(),
            "paneComposers": self.pane_composers.read(cx).ids(),
            "watchedDocs": state.fixture_sub_watch_ids(),
            "rows": child_chat_rows(state, &self.active_chat, now)
                .into_iter()
                .map(|row| serde_json::json!({
                    "id": row.id,
                    "title": row.title,
                    "status": row.status.label(),
                    "model": row.model,
                    "detail": row.detail,
                    "children": row.children,
                }))
                .collect::<Vec<_>>(),
            "sidebarOrder": self.sidebar_visible_order(cx),
            "collapsedChildChats": self.settings.collapsed_child_chats,
        })
    }
}
