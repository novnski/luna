//! Live linked activity from engine-owned child metadata. Tool output is not
//! parsed into navigation authority. Each visible card observes its own state
//! without rebuilding the parent's transcript when a child publishes a frame.
//!
//! Layout follows the transcript's own subagent chip card (same frame, icon
//! tile, 12px type): a header line — provider mark in the tile, title, a
//! quiet "New thread" type badge (a child-created record is always a fresh
//! child), status dot + status + elapsed — and one detail line under the
//! title: friendly model · reasoning, the single-line prompt/result preview,
//! then the Open chat / Show in split actions. At narrower widths the actions
//! and then the status wrap to their own lines rather than squeezing the
//! title, model or labels.
use super::*;
use crate::AccessibleButton as _;
use crate::shell::child_chats::{ChildChatStatus, child_chat_status, provider_label};

#[derive(Clone, PartialEq)]
struct Snapshot {
    title: String,
    harness: Option<luna_proto::HarnessId>,
    /// Display label for `config.model`: a curated catalog label when one
    /// matches (Claude's dated/versioned ids), else the raw id. Never hidden
    /// behind a tooltip — this renders as visible text.
    model_label: Option<String>,
    reasoning_label: Option<&'static str>,
    /// One-line preview: the initial prompt while the child is live, else
    /// `last_message_preview` (outcome summaries are compacted to empty once
    /// delivered — see `child_threads::compact_child_outcomes`), falling back
    /// to the original prompt when no preview is recorded yet.
    preview: String,
    status: ChildChatStatus,
    /// The child's own live turn start, only meaningful while `status` is
    /// `Running`. Absence must never be papered over with an invented timer.
    live_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// A frozen, already-settled duration for a terminal status, derived from
    /// the matching outcome's `created_at_ms`/`completed_at_ms`. `None` when
    /// an older recorded row has no reliable end time — shown as "no
    /// duration" rather than a fabricated or ever-growing value.
    settled_duration_ms: Option<i64>,
}

/// Codex ids are typically `gpt-<version>[-<suffix>...]` (`gpt-5-codex`,
/// `gpt-6.1-sol`). Codex has no static label catalog to borrow (checked:
/// `luna_harness::codex` exposes no model-label helper), so a recognized
/// shape gets a conservative, readable label ("GPT-6.1 Sol"); anything else
/// (a custom/fine-tuned id, an opaque id) is preserved exactly as given
/// rather than guessed at.
fn codex_model_label(id: &str) -> String {
    let lower = id.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("gpt-") else {
        return id.to_string();
    };
    let mut parts = rest.split('-');
    let Some(version) = parts.next().filter(|version| {
        !version.is_empty() && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
    }) else {
        return id.to_string();
    };
    let suffix: Vec<String> = parts
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if suffix.is_empty() {
        format!("GPT-{version}")
    } else {
        format!("GPT-{version} {}", suffix.join(" "))
    }
}

/// Exact-id lookup against Claude's curated static catalog (no RPC, no
/// fuzzy/typo matching) so the card shows "Fable 5.1" instead of a dated
/// model id. Codex gets [`codex_model_label`]'s conservative pattern label.
/// Any other provider (OpenCode) shows its own id as-is — still readable,
/// never hidden.
fn friendly_model_label(harness: luna_proto::HarnessId, id: &str) -> String {
    match harness {
        luna_proto::HarnessId::ClaudeCode | luna_proto::HarnessId::Mock => {
            fn normalize(value: &str) -> String {
                value
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase()
            }
            let needle = normalize(id);
            luna_harness::claude::catalog::static_models()
                .into_iter()
                .find(|model| normalize(&model.id) == needle)
                .map(|model| model.label)
                .unwrap_or_else(|| id.to_string())
        }
        luna_proto::HarnessId::Codex => codex_model_label(id),
        _ => id.to_string(),
    }
}

fn snapshot(state: &AppState, parent: &str, child_id: &str) -> Option<Snapshot> {
    let chat = state.chats.iter().find(|chat| chat.id == child_id)?;
    let child = chat
        .child_thread
        .as_ref()
        .filter(|child| child.parent_task_id == parent)?;
    let status = child_chat_status(state, chat, chrono::Utc::now());
    let harness = chat.config.as_ref().map(|config| config.harness);
    let model_label = chat.config.as_ref().and_then(|config| {
        config
            .model
            .as_deref()
            .map(|id| friendly_model_label(config.harness, id))
    });
    let reasoning_label = chat
        .config
        .as_ref()
        .and_then(|config| config.reasoning)
        .map(crate::pickers::reasoning_label);
    let settled = matches!(
        status,
        ChildChatStatus::Completed | ChildChatStatus::Failed | ChildChatStatus::Interrupted
    );
    let live_started_at = (!settled)
        .then(|| state.session_for(child_id))
        .flatten()
        .and_then(|session| session.started_at)
        .filter(|_| matches!(status, ChildChatStatus::Running));
    // Same "latest outcome" selection as `child_chat_status` so the duration
    // shown always matches the outcome that produced the displayed status.
    let settled_duration_ms = settled
        .then(|| {
            child
                .outcomes
                .iter()
                .max_by_key(|outcome| outcome.created_at_ms)
        })
        .flatten()
        .and_then(|outcome| {
            outcome
                .completed_at_ms
                .map(|end| (end - outcome.created_at_ms).max(0))
        });
    let preview_source = if settled {
        chat.last_message_preview
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(child.initial_prompt.as_str())
    } else {
        child.initial_prompt.as_str()
    };
    Some(Snapshot {
        title: chat.title.clone().unwrap_or_else(|| "Agent task".into()),
        harness,
        model_label,
        reasoning_label,
        preview: single_line(preview_source).chars().take(200).collect(),
        status,
        live_started_at,
        settled_duration_ms,
    })
}

/// Elapsed time text, or `None` when nothing reliable is known — never an
/// invented or ever-growing value for an old row missing an end time.
fn duration_text(card: &Snapshot, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    if matches!(card.status, ChildChatStatus::Running) {
        return card
            .live_started_at
            .map(|started| now.signed_duration_since(started).num_milliseconds().max(0) as u64)
            .map(crate::native_agents::duration_label);
    }
    card.settled_duration_ms
        .map(|ms| crate::native_agents::duration_label(ms.max(0) as u64))
}

/// A child-created record is always a fresh child conversation with no
/// inherited context: LUNA's "New thread" (never a context-carrying Side
/// chat).
const NEW_THREAD: &str = "New thread";

/// The conversation-type badge the `#` picker uses (the tab strip's Main
/// badge geometry in the chip tiles' neutral wash): a quiet label, never a
/// selection, provider or status cue, and it never shrinks.
fn type_badge(theme: &Theme, label: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .h(crate::typography::ui_rems(16.0))
        .px(crate::typography::ui_rems(5.0))
        .flex()
        .items_center()
        .rounded(px(crate::theme::radius::XS))
        .bg(crate::theme::ink(0.06))
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.content_muted)
        .child(label)
}

pub(super) struct AgentCard {
    state: Entity<AppState>,
    owner: gpui::WeakEntity<Transcript>,
    parent: String,
    child_id: String,
    generation: u64,
    snapshot: Option<Snapshot>,
    _observe: Subscription,
    /// Owned by this card alone: notifies only this entity, never the parent
    /// transcript, and is torn down as soon as `status` leaves `Running`.
    tick: Option<Task<()>>,
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
            this.sync_tick(cx);
        });
        let mut this = Self {
            state,
            owner,
            parent,
            child_id,
            generation,
            snapshot: initial,
            _observe: observe,
            tick: None,
        };
        this.sync_tick(cx);
        this
    }

    /// Keep a one-second self-notify alive exactly while the card shows a
    /// live elapsed timer, so the card's own render stays current without any
    /// parent-transcript invalidation. Stops itself the moment the snapshot
    /// is gone or `status` is no longer `Running`.
    fn sync_tick(&mut self, cx: &mut Context<Self>) {
        let ticking = matches!(
            self.snapshot.as_ref().map(|snapshot| snapshot.status),
            Some(ChildChatStatus::Running)
        );
        if !ticking {
            self.tick = None;
            return;
        }
        if self.tick.is_some() {
            return;
        }
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let live = this.update(cx, |this, cx| {
                    cx.notify();
                    matches!(
                        this.snapshot.as_ref().map(|snapshot| snapshot.status),
                        Some(ChildChatStatus::Running)
                    )
                });
                if !matches!(live, Ok(true)) {
                    break;
                }
            }
        }));
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
        let Some(card) = self.snapshot.clone() else {
            return div().into_any_element();
        };
        let now = chrono::Utc::now();
        let (icon_path, tint) = card
            .harness
            .map(crate::pickers::harness_brand_icon)
            .unwrap_or((crate::icons::WIDGET, None));
        let provider_name = card.harness.map(provider_label).unwrap_or("Agent");
        let status_label = card.status.label();
        let status_color = card.status.color(&theme);
        let duration = duration_text(&card, now);

        // Provider mark in the chip card's icon tile (`size-[18px]
        // rounded-[5px] bg-white/[0.08]`); the label carries provider and
        // status so neither is shape- or color-only.
        let identity = div()
            .id("agent-card-provider-identity")
            .aria_label(format!(
                "{provider_name} provider, {NEW_THREAD}, {status_label}"
            ))
            .size(crate::typography::ui_rems(18.0))
            .flex_none()
            .rounded(px(crate::theme::radius::SM))
            .bg(crate::theme::ink(0.08))
            .flex()
            .items_center()
            .justify_center()
            .child(
                crate::icons::icon(icon_path)
                    .size(crate::typography::ui_rems(12.0))
                    .text_color(tint.unwrap_or(theme.content_muted)),
            );

        // Status reads like the sidebar: a colored dot and quiet text; only a
        // failure tints its words. The live/frozen elapsed time trails it.
        let status = div()
            .flex()
            .items_center()
            .flex_none()
            .ml_auto()
            .gap(crate::typography::ui_rems(6.0))
            .text_color(theme.content_muted)
            .child(
                div()
                    .size(crate::typography::ui_rems(6.0))
                    .flex_none()
                    .rounded_full()
                    .bg(status_color),
            )
            .child(
                div()
                    .when(matches!(card.status, ChildChatStatus::Failed), |label| {
                        label.text_color(theme.readable_tone(theme.danger, 1.0))
                    })
                    .child(status_label),
            )
            .when_some(duration, |status, duration| {
                status
                    .child(div().text_color(theme.content_faint).child("·"))
                    .child(div().child(duration))
            });

        // Tile, title and type stay together; the status cluster wraps to
        // its own (right-aligned) line before the title drops below ~80px
        // (tile + badge take ~100 of the 180).
        let identity_line = div()
            .flex_1()
            .min_w(crate::typography::ui_rems(180.0))
            .h(px(TOOL_LABEL_LINE_HEIGHT + 2.0))
            .flex()
            .items_center()
            .gap(crate::typography::ui_rems(8.0))
            .child(identity)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(card.title.clone()),
            )
            .child(type_badge(&theme, NEW_THREAD));
        let header = div()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(crate::typography::ui_rems(8.0))
            .gap_y(px(2.0))
            .child(identity_line)
            .child(status);

        // Friendly model + reasoning stay visible text (never tooltip-only).
        let mut detail_parts: Vec<String> = Vec::new();
        if let Some(model) = &card.model_label {
            detail_parts.push(model.clone());
        }
        if let Some(reasoning) = card.reasoning_label {
            detail_parts.push(reasoning.to_string());
        }
        let detail = detail_parts.join(" · ");

        // Model · reasoning is never truncated by the preview: it keeps its
        // width, the preview (with its leading separator, so a wrap never
        // strands a dot) takes what is left down to ~88px, and the actions
        // wrap below before either is squeezed further.
        let has_detail = !detail.is_empty();
        let model = has_detail.then(|| {
            div()
                .flex_none()
                .max_w_full()
                .truncate()
                .text_color(theme.content_muted)
                .child(detail)
        });
        let preview = div()
            .flex_1()
            .min_w(crate::typography::ui_rems(88.0))
            .flex()
            .items_center()
            .gap(crate::typography::ui_rems(6.0))
            .when(has_detail, |preview| {
                preview.child(div().flex_none().text_color(theme.content_faint).child("·"))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.readable_muted(0.8))
                    .child(card.preview.clone()),
            );

        // Small labeled actions in the chip's trailing-tile language: muted
        // until hovered, a soft wash, never a raised button.
        let (muted, faint, text) = (theme.content_muted, theme.content_faint, theme.text);
        let card = cx.entity_id();
        let action = move |id: &'static str, label: &'static str, glyph: &'static str| {
            div()
                .id(id)
                .h(crate::typography::ui_rems(20.0))
                .px(crate::typography::ui_rems(6.0))
                .flex()
                .items_center()
                .gap(crate::typography::ui_rems(4.0))
                .flex_none()
                .rounded(px(crate::theme::radius::SM))
                .cursor_pointer()
                .hover_fill_fg(
                    format!("tl-{id}-{card}"),
                    gpui::transparent_black(),
                    crate::theme::fill(crate::theme::FillTier::Tertiary),
                    muted,
                    text,
                )
                .child(
                    crate::icons::icon(glyph)
                        .size(crate::typography::ui_rems(12.0))
                        .text_color(faint),
                )
                .child(label)
        };
        let actions = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(2.0))
            .child(
                action("agent-card-open", "Open chat", crate::icons::ARROW_UP_RIGHT)
                    .accessible_button("Open agent chat", theme.accent)
                    .on_click(cx.listener(|this, _, _, cx| this.open(false, cx))),
            )
            .child(
                action(
                    "agent-card-split",
                    "Show in split",
                    crate::icons::SPLIT_COLUMNS,
                )
                .accessible_button("Show agent chat in split", theme.accent)
                .on_click(cx.listener(|this, _, _, cx| this.open(true, cx))),
            );

        // Aligned under the title (tile 18px + 8px gap).
        let details = div()
            .pl(crate::typography::ui_rems(26.0))
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(crate::typography::ui_rems(6.0))
            .gap_y(px(2.0))
            .children(model)
            .child(preview)
            .child(actions);

        div()
            .id(SharedString::from(format!("agent-card-{}", self.child_id)))
            .w_full()
            .min_w_0()
            .px(crate::typography::ui_rems(8.0))
            .py(crate::typography::ui_rems(6.0))
            .rounded(px(crate::theme::radius::LG))
            .border_1()
            .border_color(crate::theme::hairline(0.07))
            .bg(crate::theme::ink(0.03))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .text_size(crate::typography::ui_rems(TOOL_LABEL_SIZE))
            .line_height(crate::typography::ui_rems(TOOL_LABEL_LINE_HEIGHT))
            .child(header)
            .child(details)
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
