//! Ticket card (A8): key, title, the engine's observed phase, the Needs
//! attention reason, provider and project when useful, PR/check evidence,
//! last meaningful activity and the reviewer marker. The card is the drag
//! source, a keyboard focus stop and the context-menu target.
//!
//! Nothing here derives state. Every badge renders what the engine reported
//! (`WorkItemView`), and meaning never rides on colour alone: each badge
//! carries a glyph and a word. The column is intent, the phase is evidence,
//! so a Backlog card that is still stopping (or cannot be observed) says so
//! instead of reading as stopped.
use gpui::{
    AnyElement, AppContext as _, Context, ElementId, FontWeight, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};
use luna_proto::{
    BoardAdmissionReason, BoardAttentionKind, BoardAttentionReason, BoardChatRole, BoardColumn,
    BoardPhase, WorkItemView,
};

use super::page::{BoardCardDrag, BoardPage, CardEvidence};
use crate::icons;
use crate::theme::{FillTier, Theme};
use crate::typography::ui_rems;

/// How a badge is toned. Tone is a secondary cue only: the glyph and the
/// word carry the meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Nothing is running (not started, idle, queued).
    Quiet,
    /// An agent is actively working on the current generation.
    Running,
    /// A stop is pending or unconfirmed, or the state cannot be observed.
    Caution,
    Danger,
    Success,
    /// Stopped: held, not running.
    Neutral,
}

impl Tone {
    pub(crate) fn color(self, theme: &Theme) -> Hsla {
        match self {
            Tone::Quiet | Tone::Neutral => theme.readable_muted(0.9),
            Tone::Running => theme.readable_tone(theme.accent, 1.0),
            Tone::Caution => theme.readable_tone(theme.warning, 0.95),
            Tone::Danger => theme.readable_tone(theme.danger, 0.95),
            Tone::Success => theme.readable_tone(theme.success, 0.95),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Tone::Quiet => "quiet",
            Tone::Running => "running",
            Tone::Caution => "caution",
            Tone::Danger => "danger",
            Tone::Success => "success",
            Tone::Neutral => "neutral",
        }
    }
}

/// The phase as the card shows it: a word, a glyph and a longer description
/// for assistive technology and tooltips.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhaseBadge {
    pub label: String,
    pub glyph: Option<&'static str>,
    pub tone: Tone,
    pub description: &'static str,
}

/// The observed phase in the context of the card's column. The engine's
/// phase is never replaced; the column only adds honesty where the two could
/// be misread together (a Backlog card that has not finished stopping).
pub(crate) fn phase_badge(view: &WorkItemView) -> PhaseBadge {
    let column = view.item.column;
    let (label, glyph, tone, description): (String, Option<&'static str>, Tone, &'static str) =
        match view.phase {
            BoardPhase::NotStarted => (
                "Not started".into(),
                None,
                Tone::Quiet,
                "No chat has been started for this ticket",
            ),
            BoardPhase::Queued => (
                match &view.queue_reason {
                    Some(reason) => format!("Queued · {}", queue_reason_label(reason)),
                    None => "Queued".into(),
                },
                Some(icons::CLOCK_CIRCLE),
                Tone::Quiet,
                "Admitted and waiting to start",
            ),
            BoardPhase::Starting => (
                "Starting".into(),
                Some(icons::PLAY),
                Tone::Running,
                "The agent is starting",
            ),
            BoardPhase::Working => (
                "Working".into(),
                Some(icons::BOLT_FAST),
                Tone::Running,
                "The agent is working",
            ),
            BoardPhase::Checking => (
                "Checking".into(),
                Some(icons::CHECKLIST),
                Tone::Running,
                "Required checks are running",
            ),
            BoardPhase::Reviewing => (
                "Reviewing".into(),
                Some(icons::SHIELD_CHECK),
                Tone::Running,
                "A reviewer side chat is checking the current result",
            ),
            BoardPhase::Fixing => (
                "Fixing".into(),
                Some(icons::PEN),
                Tone::Running,
                "The agent is addressing review findings",
            ),
            BoardPhase::Idle => (
                "Idle".into(),
                Some(icons::PAUSE),
                Tone::Quiet,
                "The chat is idle without a finished result",
            ),
            BoardPhase::WaitingForStop => (
                "Waiting for stop".into(),
                Some(icons::STOP_CIRCLE),
                Tone::Caution,
                "Ready again; continues after the previous run has stopped",
            ),
            BoardPhase::StopRequested => (
                "Stop requested".into(),
                Some(icons::STOP_CIRCLE),
                Tone::Caution,
                "Stop was requested; the agent may still be running",
            ),
            BoardPhase::Stopping => (
                "Stopping".into(),
                Some(icons::STOP_CIRCLE),
                Tone::Caution,
                "The agent is stopping and may still be running",
            ),
            BoardPhase::Stopped => (
                "Stopped".into(),
                Some(icons::STOP),
                Tone::Neutral,
                "The agent stopped; its chat and changes are kept",
            ),
            BoardPhase::Failed => (
                "Failed".into(),
                Some(icons::CLOSE_CIRCLE),
                Tone::Danger,
                "The last run failed",
            ),
            BoardPhase::Complete => (
                "Complete".into(),
                Some(icons::CHECK),
                Tone::Success,
                "Checks and review passed for the current result",
            ),
            BoardPhase::CleanupPending => (
                "Done · cleanup pending".into(),
                Some(icons::ARCHIVE_MINIMALISTIC),
                Tone::Success,
                "Passed; reviewer chats are still being closed",
            ),
            BoardPhase::Unknown => (
                if column == BoardColumn::Backlog {
                    "Unknown · stop not confirmed".into()
                } else {
                    "Unknown".into()
                },
                Some(icons::INFO_CIRCLE),
                Tone::Caution,
                "The agent's state cannot be observed right now",
            ),
        };
    PhaseBadge {
        label,
        glyph,
        tone,
        description,
    }
}

fn queue_reason_label(reason: &BoardAdmissionReason) -> &'static str {
    match reason {
        BoardAdmissionReason::ProjectCapacity { .. } => "project capacity",
        BoardAdmissionReason::HostCapacity { .. } => "host capacity",
        BoardAdmissionReason::ProviderCapacity { .. } => "provider capacity",
        BoardAdmissionReason::Budget { .. } => "budget",
        BoardAdmissionReason::Dependency { .. } => "blocked",
        BoardAdmissionReason::CheckoutBusy { .. } => "checkout busy",
        BoardAdmissionReason::WaitingForStop { .. } => "waiting for stop",
        BoardAdmissionReason::HostUnavailable { .. } => "host offline",
        BoardAdmissionReason::SettingsUnresolved { .. } => "settings needed",
    }
}

/// A Needs-attention reason in a few words.
pub(crate) fn attention_label(kind: BoardAttentionKind) -> &'static str {
    match kind {
        BoardAttentionKind::Question => "Question waiting",
        BoardAttentionKind::Approval => "Approval needed",
        BoardAttentionKind::ProviderFailure => "Provider failed",
        BoardAttentionKind::AuthRequired => "Sign-in required",
        BoardAttentionKind::HostUnavailable => "Host unavailable",
        BoardAttentionKind::CompatibilityBlocked => "Not compatible",
        BoardAttentionKind::SettingsUnresolved => "Settings needed",
        BoardAttentionKind::ModelUnavailable => "Model unavailable",
        BoardAttentionKind::ReviewBlocked => "Review blocked",
        BoardAttentionKind::ReviewUnavailable => "Review unavailable",
        BoardAttentionKind::StopUnknown => "Stop not confirmed",
        BoardAttentionKind::DeliveryUnknown => "Delivery unconfirmed",
        BoardAttentionKind::DependencyBlocked => "Blocked by dependency",
        BoardAttentionKind::CleanupFailed => "Cleanup failed",
        BoardAttentionKind::AgentBlocked => "Agent blocked",
        BoardAttentionKind::Other => "Needs attention",
    }
}

/// The attention line's text: the first reason's label and detail, plus a
/// count when there are more.
pub(crate) fn attention_text(reasons: &[BoardAttentionReason]) -> Option<String> {
    let first = reasons.first()?;
    let mut text = attention_label(first.kind).to_owned();
    let detail = first.detail.trim();
    if !detail.is_empty() {
        text.push_str(": ");
        text.push_str(&luna_proto::view::single_line(detail));
    }
    if reasons.len() > 1 {
        text.push_str(&format!(" (+{} more)", reasons.len() - 1));
    }
    Some(text)
}

/// Where the ticket's working chat lives, in words.
pub(crate) fn chat_location(view: &WorkItemView) -> String {
    match &view.working_chat {
        None => "No chat yet".into(),
        Some(chat) => match chat.role {
            BoardChatRole::Main => "Main chat".into(),
            BoardChatRole::Side => match (&chat.main_parent_title, chat.parent_available) {
                (_, false) => "Side chat · parent unavailable".into(),
                (Some(title), true) => {
                    format!("Side chat · {}", luna_proto::view::single_line(title))
                }
                (None, true) => "Side chat".into(),
            },
        },
    }
}

/// Relative age of the last meaningful activity ("5m", "now").
pub(crate) fn activity_age(at_ms: i64) -> Option<String> {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms)?;
    (at_ms > 0).then(|| crate::state::format_time_ago(at, chrono::Utc::now()))
}

/// The provider the card names: the working chat's actual harness, else
/// the ticket's requested one (inherited settings name nothing).
pub(crate) fn provider_of(view: &WorkItemView) -> Option<luna_proto::HarnessId> {
    view.working_chat
        .as_ref()
        .and_then(|chat| chat.harness)
        .or(view.item.implementation.harness)
}

/// The accessible name of a card: everything a sighted user reads at a
/// glance, in order.
pub(crate) fn card_label(view: &WorkItemView, project: Option<&str>) -> String {
    let badge = phase_badge(view);
    let mut label = format!(
        "{}, {}, {}, {}",
        view.item.key,
        view.item.title,
        view.item.column.label(),
        badge.label
    );
    if let Some(attention) = attention_text(&view.attention) {
        label.push_str(", needs attention: ");
        label.push_str(&attention);
    }
    if let Some(project) = project {
        label.push_str(", project ");
        label.push_str(project);
    }
    if view.item.archived {
        label.push_str(", archived");
    }
    label
}

/// A small glyph + word badge.
pub(crate) fn badge(
    theme: &Theme,
    glyph: Option<&'static str>,
    label: impl Into<SharedString>,
    tone: Tone,
) -> gpui::Div {
    let color = tone.color(theme);
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(ui_rems(4.0))
        .min_w_0()
        .text_size(ui_rems(11.0))
        .line_height(ui_rems(14.0))
        .text_color(color)
        .child(match glyph {
            Some(glyph) => icons::icon(glyph)
                .size(ui_rems(12.0))
                .flex_none()
                .text_color(color)
                .into_any_element(),
            // A ring, never a filled dot: not started is the absence of a run.
            None => div()
                .flex_none()
                .size(ui_rems(8.0))
                .rounded_full()
                .border_1()
                .border_color(color)
                .into_any_element(),
        })
        .child(div().min_w_0().truncate().child(label.into()))
}

/// The status mark beside a column title: Backlog a dashed-looking ring,
/// Ready a filled accent dot, Done a filled success dot. The column name
/// beside it carries the meaning.
pub(crate) fn column_mark(column: BoardColumn, theme: &Theme) -> gpui::Div {
    let mark = div()
        .flex_none()
        .size(ui_rems(8.0))
        .rounded_full()
        .border(px(1.5));
    match column {
        BoardColumn::Backlog => mark.border_color(theme.readable_muted(0.8)),
        BoardColumn::Ready => mark.border_color(theme.accent).bg(theme.accent),
        BoardColumn::Done => mark.border_color(theme.success).bg(theme.success),
    }
}

impl BoardPage {
    /// One ticket card. `drop_line` paints the insertion line above it while
    /// a card is dragged over this slot.
    pub(super) fn render_card(
        &mut self,
        view: &WorkItemView,
        drop_line: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item = &view.item;
        let id = item.id.clone();
        let focus = self.card_focus(&id, cx);
        let selected = self.selected.as_deref() == Some(id.as_str());
        let badge_info = phase_badge(view);
        let project = self.project_label_for(item, cx);
        let label = card_label(view, project.as_deref());
        let attention = attention_text(&view.attention);
        let provider = provider_of(view);
        let evidence = self.evidence.get(&id).cloned();
        let age = activity_age(view.last_activity_ms);
        let reviewing = view.phase == BoardPhase::Reviewing;
        let dragging = self.dragging.as_deref() == Some(id.as_str());
        let ring = theme.focus_ring();
        let card_key = SharedString::from(format!("board-card-{id}"));
        let drag = BoardCardDrag {
            work_item_id: id.clone(),
            project_id: item.project_id.clone(),
            from: item.column,
            key: item.key.clone(),
            title: item.title.clone(),
        };

        let top = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(5.0))
            .text_size(ui_rems(11.0))
            .line_height(ui_rems(14.0))
            .text_color(theme.readable_muted(0.85))
            .child(
                icons::icon(icons::TICKET)
                    .size(ui_rems(12.0))
                    .flex_none()
                    .text_color(theme.readable_muted(0.8)),
            )
            .child(
                div()
                    .flex_none()
                    .child(SharedString::from(item.key.clone())),
            )
            .when_some(project.clone(), |row, project| {
                row.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.readable_muted(0.7))
                        .child(SharedString::from(format!("· {project}"))),
                )
            })
            .child(div().flex_1())
            .when_some(provider, |row, harness| {
                let (glyph, tint) = crate::pickers::harness_brand_icon(harness);
                row.child(
                    div()
                        .id(SharedString::from(format!("board-card-provider-{id}")))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(ui_rems(3.0))
                        .child(
                            icons::icon(glyph)
                                .size(ui_rems(12.0))
                                .flex_none()
                                .text_color(tint.unwrap_or(theme.readable_muted(0.85))),
                        )
                        .child(SharedString::from(
                            super::super::child_chats::provider_label(harness),
                        )),
                )
            })
            .child(
                crate::button::icon_button(
                    format!("board-card-menu-{id}"),
                    icons::MORE_HORIZONTAL,
                    crate::button::ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label(format!("Actions for {}", item.key))
                .tooltip(crate::popover::text_tooltip("Actions"))
                .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.select(&id, true, window, cx);
                        this.open_card_menu(&id, None, window, cx);
                    }
                })),
            );

        let title = div()
            .min_w_0()
            .text_size(ui_rems(13.0))
            .line_height(ui_rems(18.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text)
            .line_clamp(3)
            .child(SharedString::from(item.title.clone()));

        let status = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_x(ui_rems(10.0))
            .gap_y(ui_rems(4.0))
            .child(
                badge(
                    theme,
                    badge_info.glyph,
                    badge_info.label.clone(),
                    badge_info.tone,
                )
                .id(SharedString::from(format!("board-card-phase-{id}")))
                .tooltip(crate::popover::text_tooltip(badge_info.description))
                .tooltip_show_delay(crate::popover::TOOLTIP_DELAY),
            )
            .when(reviewing, |row| {
                row.child(badge(
                    theme,
                    Some(icons::SHIELD_CHECK),
                    "Reviewer active",
                    Tone::Running,
                ))
            })
            .when(item.archived, |row| {
                row.child(badge(
                    theme,
                    Some(icons::ARCHIVE_MINIMALISTIC),
                    "Archived",
                    Tone::Quiet,
                ))
            });

        let attention_line = attention.clone().map(|text| {
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(ui_rems(4.0))
                .px(ui_rems(6.0))
                .py(ui_rems(4.0))
                .rounded(px(crate::theme::radius::SM))
                .bg(theme.warning.opacity(0.08))
                .text_size(ui_rems(11.0))
                .line_height(ui_rems(14.0))
                .text_color(Tone::Caution.color(theme))
                .child(
                    icons::icon(icons::DANGER_TRIANGLE)
                        .size(ui_rems(12.0))
                        .flex_none()
                        .mt(px(1.0))
                        .text_color(Tone::Caution.color(theme)),
                )
                .child(
                    div()
                        .min_w_0()
                        .line_clamp(2)
                        .child(SharedString::from(text)),
                )
        });

        let mut meta: Vec<AnyElement> = Vec::new();
        meta.push(
            div()
                .min_w_0()
                .truncate()
                .child(SharedString::from(chat_location(view)))
                .into_any_element(),
        );
        if let Some(issue) = &view.issue {
            meta.push(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(issue.display.clone()))
                    .into_any_element(),
            );
        }
        if let Some(evidence) = &evidence {
            meta.extend(evidence_badges(evidence, theme));
        }
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(8.0))
            .pt(ui_rems(6.0))
            .border_t_1()
            .border_color(theme.separator(false))
            .text_size(ui_rems(11.0))
            .line_height(ui_rems(14.0))
            .text_color(theme.readable_muted(0.8))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_x(ui_rems(8.0))
                    .gap_y(ui_rems(2.0))
                    .children(meta),
            )
            .when_some(age, |row, age| {
                row.child(div().flex_none().child(SharedString::from(age)))
            });

        let card = div()
            .id(ElementId::from(card_key.clone()))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(label))
            .aria_description(SharedString::from(badge_info.description))
            .aria_selected(selected)
            .aria_keyshortcuts(
                "Enter opens; Alt+Right moves to Ready; Alt+Left moves to Backlog; \
                 Alt+Up/Down reorders; Cmd+Enter opens the chat; Shift+F10 shows actions",
            )
            .track_focus(&focus)
            .tab_index(0)
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .px(ui_rems(10.0))
            .py(ui_rems(9.0))
            .rounded(px(crate::theme::radius::LG))
            .border_1()
            .border_color(if selected {
                theme.accent.opacity(0.7)
            } else {
                theme.separator(true)
            })
            .bg(if selected {
                theme.accent_wash
            } else {
                theme.fill(FillTier::Quaternary)
            })
            .cursor_pointer()
            .when(dragging, |card| card.opacity(0.45))
            .when(item.archived, |card| card.opacity(0.7))
            .hover(|style| style.border_color(theme.accent.opacity(0.45)))
            .focus_visible(move |style| crate::theme::focus_ring(style, ring))
            .on_key_down(cx.listener({
                let id = id.clone();
                move |this, event: &KeyDownEvent, window, cx| {
                    if this.on_card_key(&id, event, window, cx) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, window, cx| {
                    this.select(&id, true, window, cx);
                    this.open_detail(&id, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = id.clone();
                    move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.select(&id, true, window, cx);
                        this.open_card_menu(&id, Some(event.position), window, cx);
                    }
                }),
            )
            .when(crate::click_activation_drag_enabled(), |card| {
                card.on_drag(drag, |drag, _offset, _window, cx| {
                    cx.stop_propagation();
                    let drag = drag.clone();
                    cx.new(|_| BoardCardGhost {
                        key: drag.key,
                        title: drag.title,
                    })
                })
            })
            .child(top)
            .child(title)
            .child(status)
            .children(attention_line)
            .child(footer);

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(4.0))
            .when(drop_line, |slot| slot.child(drop_indicator(theme)))
            .child(card)
            .into_any_element()
    }
}

/// The insertion line shown where a dragged card would land.
pub(crate) fn drop_indicator(theme: &Theme) -> gpui::Div {
    div().w_full().h(px(2.0)).rounded_full().bg(theme.accent)
}

/// PR and check badges from the ticket's current (non-historical) outcomes.
fn evidence_badges(evidence: &CardEvidence, theme: &Theme) -> Vec<AnyElement> {
    let mut badges = Vec::new();
    if let Some(pr) = &evidence.pull_request {
        badges.push(
            badge(theme, Some(icons::PULL_REQUEST), pr.clone(), Tone::Quiet).into_any_element(),
        );
    }
    if evidence.checks_total > 0 {
        let (tone, glyph) = if evidence.checks_failed > 0 {
            (Tone::Danger, icons::CLOSE_CIRCLE)
        } else if evidence.checks_passed == evidence.checks_total {
            (Tone::Success, icons::CHECK)
        } else {
            (Tone::Quiet, icons::CLOCK_CIRCLE)
        };
        let label = if evidence.checks_failed > 0 {
            format!(
                "{} of {} checks failed",
                evidence.checks_failed, evidence.checks_total
            )
        } else {
            format!(
                "{}/{} checks",
                evidence.checks_passed, evidence.checks_total
            )
        };
        badges.push(badge(theme, Some(glyph), label, tone).into_any_element());
    }
    badges
}

/// What follows the pointer while a card is dragged: its key and title.
pub(crate) struct BoardCardGhost {
    key: String,
    title: String,
}

impl gpui::Render for BoardCardGhost {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .max_w(ui_rems(260.0))
            .px(ui_rems(10.0))
            .py(ui_rems(7.0))
            .rounded(px(crate::theme::radius::LG))
            .border_1()
            .border_color(theme.accent.opacity(0.6))
            .bg(theme.surface_raised)
            .shadow(crate::theme::menu_shadows())
            .flex()
            .flex_col()
            .gap(ui_rems(2.0))
            .child(
                div()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.readable_muted(0.85))
                    .child(SharedString::from(self.key.clone())),
            )
            .child(
                div()
                    .text_size(ui_rems(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .truncate()
                    .child(SharedString::from(self.title.clone())),
            )
    }
}
