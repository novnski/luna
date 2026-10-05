//! Live linked activity from engine-owned child metadata. Tool output is not
//! parsed into navigation authority. Each visible card observes its own state
//! without rebuilding the parent's transcript when a child publishes a frame.
use super::*;
use crate::AccessibleButton as _;
use crate::shell::child_chats::{ChildChatStatus, child_chat_status, provider_label};

#[derive(Clone, PartialEq)]
struct Snapshot {
    title: String,
    execution: String,
    brief: String,
    status: ChildChatStatus,
}

fn snapshot(state: &AppState, parent: &str, child_id: &str) -> Option<Snapshot> {
    let chat = state.chats.iter().find(|chat| chat.id == child_id)?;
    let child = chat
        .child_thread
        .as_ref()
        .filter(|child| child.parent_task_id == parent)?;
    let execution = chat
        .config
        .as_ref()
        .map(|config| {
            let mut parts = vec![provider_label(config.harness).to_owned()];
            if let Some(model) = &config.model {
                parts.push(model.clone());
            }
            if let Some(reasoning) = config.reasoning {
                parts.push(crate::pickers::reasoning_label(reasoning).into());
            }
            parts.join(" · ")
        })
        .unwrap_or_else(|| "Agent".into());
    Some(Snapshot {
        title: chat.title.clone().unwrap_or_else(|| "Agent task".into()),
        execution,
        brief: child
            .initial_prompt
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(180)
            .collect(),
        status: child_chat_status(state, chat, chrono::Utc::now()),
    })
}

pub(super) struct AgentCard {
    state: Entity<AppState>,
    owner: gpui::WeakEntity<Transcript>,
    parent: String,
    child_id: String,
    generation: u64,
    snapshot: Option<Snapshot>,
    _observe: Subscription,
}

impl AgentCard {
    fn new(
        state: Entity<AppState>,
        owner: gpui::WeakEntity<Transcript>,
        parent: String,
        child_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = state.read(cx).engine_generation();
        let initial = snapshot(state.read(cx), &parent, &child_id);
        let observe = cx.observe(&state, |this: &mut Self, _, cx| {
            let state = this.state.read(cx);
            let next = (state.engine_generation() == this.generation)
                .then(|| snapshot(state, &this.parent, &this.child_id))
                .flatten();
            if this.snapshot != next {
                this.snapshot = next;
                cx.notify();
            }
        });
        Self {
            state,
            owner,
            parent,
            child_id,
            generation,
            snapshot: initial,
            _observe: observe,
        }
    }

    fn open(&self, split: bool, cx: &mut Context<Self>) {
        if self.state.read(cx).engine_generation() != self.generation || self.snapshot.is_none() {
            return;
        }
        let event = TranscriptEvent::OpenAgentChat {
            task_id: self.child_id.clone(),
            parent_task_id: self.parent.clone(),
            split,
        };
        let _ = self.owner.update(cx, |_, cx| cx.emit(event));
    }
}

impl Render for AgentCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Some(card) = &self.snapshot else {
            return div().into_any_element();
        };
        let actions = div()
            .flex()
            .gap(px(6.0))
            .flex_wrap()
            .child(
                crate::settings::widgets::ghost_action(&theme)
                    .id("agent-card-open")
                    .accessible_button("Open agent chat", theme.accent)
                    .child("Open chat")
                    .on_click(cx.listener(|this, _, _, cx| this.open(false, cx))),
            )
            .child(
                crate::settings::widgets::ghost_action(&theme)
                    .id("agent-card-split")
                    .accessible_button("Show agent chat in split", theme.accent)
                    .child("Show in split")
                    .on_click(cx.listener(|this, _, _, cx| this.open(true, cx))),
            );
        div()
            .id(SharedString::from(format!("agent-card-{}", self.child_id)))
            .w_full()
            .min_w_0()
            .p(px(12.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .flex()
            .flex_col()
            .gap(px(7.0))
            .text_size(crate::typography::ui_rems(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(card.title.clone()),
                    )
                    .child(
                        div()
                            .text_color(card.status.color(&theme))
                            .child(card.status.label()),
                    ),
            )
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(card.execution.clone()),
            )
            .child(div().text_color(theme.text_muted).child(card.brief.clone()))
            .child(actions)
            .into_any_element()
    }
}

impl Transcript {
    pub(super) fn agent_creation_card(
        &mut self,
        row: &Row,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if row.role != MessageRole::System || !row.turn_start {
            return None;
        }
        let child_id = row.entry_id.strip_prefix("child-created:")?;
        let parent = self.chat_id.as_ref()?;
        snapshot(self.state.read(cx), parent, child_id)?;
        let card = self
            .agent_cards
            .entry(child_id.to_owned())
            .or_insert_with(|| {
                let state = self.state.clone();
                let owner = cx.weak_entity();
                cx.new(|cx| AgentCard::new(state, owner, parent.clone(), child_id.to_owned(), cx))
            });
        Some(card.clone().into_any_element())
    }
}
