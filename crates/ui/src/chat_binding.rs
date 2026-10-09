//! Which chat a composer (and its pickers, queue, goal bar and approval
//! cards) drives.
//!
//! The main conversation composer follows [`AppState::selected_chat`]. A
//! right-pane chat view (side chat or child chat) binds the SAME composer to
//! one fixed chat id instead. Every read of "the composer's own chat" goes
//! through this type so the two never cross: a pane composer must never send,
//! steer, stop, approve or change settings on the selected chat, and the main
//! composer must keep reading exactly what it read before.
//!
//! Per-chat projections (`transcript`, `queue`, context usage, provider
//! notice, native tasks) live on `AppState` for the selected chat and in
//! [`AppState::pane_chat`] for pane-bound chats. When a pane chat happens to
//! be the selected chat too, the selected chat's live projection wins.
//!
//! A [`ChatBinding::Detached`] binding drives no chat at all: the same
//! pickers (and composer) edit settings that belong to something else — a
//! Board ticket's execution profile, a temporary ticket discussion — fixed to
//! one project and its host. It reads no chat projection and never follows
//! the selection.

use luna_doc::{QueuedMessage, SessionMessageEntry};
use luna_proto::{Chat, ContextControlState, ContextUsage, NativeTaskLedger, ProviderNotice};

use crate::state::AppState;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ChatBinding {
    /// Follow `AppState::selected_chat` (the main conversation composer).
    #[default]
    Selected,
    /// A fixed chat shown in the right pane.
    Chat(String),
    /// No chat: settings owned elsewhere (a Board ticket or discussion),
    /// fixed to this project.
    Detached { space_id: String },
}

static EMPTY_CONTROL: std::sync::LazyLock<ContextControlState> =
    std::sync::LazyLock::new(ContextControlState::default);
static EMPTY_TASKS: std::sync::LazyLock<NativeTaskLedger> =
    std::sync::LazyLock::new(NativeTaskLedger::default);

impl ChatBinding {
    /// A pane binding (fixed chat id).
    pub fn is_pane(&self) -> bool {
        matches!(self, Self::Chat(_))
    }

    /// A chat-less binding (settings owned by a ticket or discussion).
    pub fn is_detached(&self) -> bool {
        matches!(self, Self::Detached { .. })
    }

    /// The fixed chat id of a pane binding.
    pub fn pane_chat(&self) -> Option<&str> {
        match self {
            Self::Selected | Self::Detached { .. } => None,
            Self::Chat(id) => Some(id),
        }
    }

    /// The bound chat id: the selected chat, or the fixed pane chat.
    pub fn chat_id<'a>(&'a self, state: &'a AppState) -> Option<&'a str> {
        match self {
            Self::Selected => state.selected_chat.as_deref(),
            Self::Chat(id) => Some(id.as_str()),
            Self::Detached { .. } => None,
        }
    }

    /// Draft/attachment key: the bound chat id, or `""` for the new-chat canvas.
    pub fn key(&self, state: &AppState) -> String {
        self.chat_id(state).unwrap_or_default().to_string()
    }

    pub fn chat_row<'a>(&self, state: &'a AppState) -> Option<&'a Chat> {
        match self {
            Self::Selected => state.selected_chat_row(),
            Self::Chat(id) => state.chats.iter().find(|chat| chat.id == *id),
            Self::Detached { .. } => None,
        }
    }

    /// The project this composer targets: the selected project, or the
    /// bound chat's own project (falling back to the selection).
    pub fn space_id(&self, state: &AppState) -> Option<String> {
        match self {
            Self::Selected => state.selected_space.clone(),
            Self::Chat(_) => self
                .chat_row(state)
                .and_then(|chat| chat.space_id.clone())
                .or_else(|| state.selected_space.clone()),
            Self::Detached { space_id } => Some(space_id.clone()),
        }
    }

    pub fn space_row<'a>(&self, state: &'a AppState) -> Option<&'a luna_proto::Space> {
        match self {
            Self::Selected => state.selected_space_row(),
            Self::Chat(_) => match self
                .chat_row(state)
                .and_then(|chat| chat.space_id.as_deref())
            {
                Some(space) => state.space_row(space),
                None => state.selected_space_row(),
            },
            Self::Detached { space_id } => state.space_row(space_id),
        }
    }

    /// The device whose catalogs (agents, models, refs) this composer uses:
    /// the effective new-session target, or the bound chat's host.
    pub fn device_id(&self, state: &AppState) -> Option<String> {
        match self {
            Self::Selected => state.effective_device_id(),
            Self::Chat(_) => self
                .chat_row(state)
                .map(|chat| chat.device_id.clone())
                .or_else(|| state.effective_device_id()),
            // The project's own host: the only place its work runs.
            Self::Detached { space_id } => state
                .space_row(space_id)
                .map(|space| space.device_id.clone()),
        }
    }

    pub fn space_git(&self, state: &AppState) -> bool {
        self.space_row(state)
            .is_some_and(|space| space.git_detected)
    }

    /// Whether this binding reads the selected chat's live projections.
    fn reads_selected(&self, state: &AppState) -> bool {
        match self {
            Self::Selected => true,
            Self::Chat(id) => state.selected_chat.as_deref() == Some(id.as_str()),
            Self::Detached { .. } => false,
        }
    }

    /// A token that changes whenever the bound transcript's content does
    /// (slice 6). `None` when its content is not tracked by recorded frames
    /// (then callers must inspect the entries themselves).
    pub fn transcript_token(&self, state: &AppState) -> Option<u64> {
        match self {
            _ if self.reads_selected(state) => state
                .entry_generations(None)
                .map(|_| state.main_transcript_revision()),
            Self::Chat(id) => state
                .entry_generations(Some(id))
                .map(|_| state.sub_revision(id)),
            Self::Selected | Self::Detached { .. } => None,
        }
    }

    pub fn transcript<'a>(&self, state: &'a AppState) -> &'a [SessionMessageEntry] {
        match self {
            _ if self.reads_selected(state) => &state.transcript,
            Self::Chat(id) => state.sub_transcript(id),
            Self::Selected | Self::Detached { .. } => &[],
        }
    }

    pub fn queue<'a>(&self, state: &'a AppState) -> &'a [QueuedMessage] {
        match self {
            _ if self.reads_selected(state) => &state.queue,
            Self::Chat(id) => state
                .pane_chat(id)
                .map(|pane| pane.queue.as_slice())
                .unwrap_or(&[]),
            Self::Selected | Self::Detached { .. } => &[],
        }
    }

    pub fn queue_mut<'a>(&self, state: &'a mut AppState) -> Option<&'a mut Vec<QueuedMessage>> {
        if self.reads_selected(state) {
            return Some(&mut state.queue);
        }
        match self {
            Self::Chat(id) => state.pane_chat_mut(id).map(|pane| &mut pane.queue),
            Self::Selected | Self::Detached { .. } => None,
        }
    }

    pub fn context_usage(&self, state: &AppState) -> Option<ContextUsage> {
        match self {
            _ if self.reads_selected(state) => state.context_usage,
            Self::Chat(id) => state.pane_chat(id).and_then(|pane| pane.context_usage),
            Self::Selected | Self::Detached { .. } => None,
        }
    }

    pub fn context_control<'a>(&self, state: &'a AppState) -> &'a ContextControlState {
        match self {
            _ if self.reads_selected(state) => &state.context_control,
            Self::Chat(id) => state
                .pane_chat(id)
                .map(|pane| &pane.context_control)
                .unwrap_or(&EMPTY_CONTROL),
            Self::Selected | Self::Detached { .. } => &EMPTY_CONTROL,
        }
    }

    pub fn checkpointed<'a>(
        &self,
        state: &'a AppState,
    ) -> Option<&'a std::sync::Arc<std::collections::HashSet<String>>> {
        match self {
            _ if self.reads_selected(state) => state.checkpointed.as_ref(),
            Self::Chat(id) => state
                .pane_chat(id)
                .and_then(|pane| pane.checkpointed.as_ref()),
            Self::Selected | Self::Detached { .. } => None,
        }
    }

    pub fn provider_notice<'a>(&self, state: &'a AppState) -> Option<&'a ProviderNotice> {
        match self {
            _ if self.reads_selected(state) => state.provider_notice.as_ref(),
            Self::Chat(id) => state
                .pane_chat(id)
                .and_then(|pane| pane.provider_notice.as_ref()),
            Self::Selected | Self::Detached { .. } => None,
        }
    }

    pub fn native_tasks<'a>(&self, state: &'a AppState) -> &'a NativeTaskLedger {
        match self {
            _ if self.reads_selected(state) => &state.native_tasks,
            Self::Chat(id) => state
                .pane_chat(id)
                .map(|pane| &pane.native_tasks)
                .unwrap_or(&EMPTY_TASKS),
            Self::Selected | Self::Detached { .. } => &EMPTY_TASKS,
        }
    }

    /// Whether the bound chat's transcript has replayed at least once.
    pub fn transcript_replayed(&self, state: &AppState) -> bool {
        match self {
            _ if self.reads_selected(state) => state.transcript_replayed,
            Self::Chat(id) => state.pane_chat(id).is_some_and(|pane| pane.replayed),
            Self::Selected | Self::Detached { .. } => false,
        }
    }

    /// Resubscribe the bound chat's queue after a failed optimistic edit.
    pub fn refresh_queue(&self, state: &mut AppState, cx: &mut gpui::Context<AppState>) {
        match self {
            _ if self.reads_selected(state) => state.refresh_selected_queue(cx),
            Self::Chat(id) => state.refresh_pane_queue(id, cx),
            Self::Selected | Self::Detached { .. } => {}
        }
    }
}
