//! Codex goals in the composer, matching Codex's own app:
//!
//! - **Goal mode.** `/goal <objective>` sends that objective as a native goal;
//!   a bare `/goal` turns goal mode on so the next Send starts a goal instead
//!   of a message. Only that explicit command does: while goal mode is on, a
//!   "Goal" badge beside the access chip shows it and turns it off when
//!   clicked, and nothing else shows the badge. The engine runs Codex's own
//!   sequence (`thread/goal/clear`, then `thread/goal/set {status: active}`)
//!   on a loaded runtime, starting or waking it like a send, so Codex begins
//!   working immediately. The message is marked "Sent as goal" in the
//!   transcript.
//! - **The goal bar.** While a goal is unfinished, a strip stacked on the
//!   composer reads "Pursuing goal · objective · live timer" with Edit,
//!   Pause/Resume and Clear. Edit opens an inline editor (objective, budget,
//!   "Updated …", Undo, Save) bound to the chat that opened it.
//! - **Completion** is Codex's: when the model completes the goal the bar goes
//!   away and the turn shows "Goal achieved in …" (transcript goal marks).
//!
//! Every control re-checks its owning chat, one pending change disables every
//! control, and uncertain changes are never replayed.
use super::*;
use gpui::AnyElement;
use luna_proto::{
    GoalBudget, GoalOperation, GoalStatus, NativeSchedule, NativeScheduleKind, ThreadGoal,
};

#[cfg(feature = "appshots-fixture")]
#[path = "goals_e2e.rs"]
mod e2e;
#[cfg(feature = "appshots-fixture")]
pub use e2e::run_goal_card_e2e;

/// LUNA's own slash command, offered on Codex and Claude Code chats.
pub(super) const GOAL_COMMAND: &str = "goal";
/// Codex's goal-mode placeholder.
pub(super) const GOAL_PLACEHOLDER: &str =
    "Describe your goal, define measurable outcomes for best results";
/// Claude's goal is a condition its checker confirms from the conversation.
pub(super) const CLAUDE_GOAL_PLACEHOLDER: &str =
    "Describe a condition Claude can show is met, like \u{201c}all tests in test/auth pass\u{201d}";
const CLAUDE_CLEAR_BETWEEN_TURNS: &str =
    "Claude clears a goal between turns. Stop the current turn first.";
const CLAUDE_SCHEDULE_LIFETIME: &str = "Claude runs these only while this chat's Claude session is open. LUNA closes an idle session after 30 minutes and never runs them itself.";

/// Whose goal semantics a composer surface follows. Claude's `/goal` is a
/// condition checked after each turn; it has no pause, budget or in-place edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GoalProvider {
    Codex,
    Claude,
}

impl GoalProvider {
    pub(super) fn of(harness: HarnessId) -> Option<Self> {
        match harness {
            HarnessId::Codex => Some(Self::Codex),
            HarnessId::ClaudeCode => Some(Self::Claude),
            _ => None,
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Codex => GOAL_PLACEHOLDER,
            Self::Claude => CLAUDE_GOAL_PLACEHOLDER,
        }
    }

    fn mode_on_description(self) -> &'static str {
        match self {
            Self::Codex => "On: Send starts a goal Codex keeps working on until it is done.",
            Self::Claude => {
                "On: Send starts a goal Claude keeps working on until its checker confirms the condition."
            }
        }
    }
}
const MESSAGE_PLACEHOLDER: &str = "Do anything…";
/// The queue tray's corner, so stacked trays read as one family.
const CARD_RADIUS: f32 = 16.0;
const HEADER_HEIGHT: f32 = 38.0;
const GLYPH: f32 = 14.0;
/// LUNA's working glyph (3×3 accent cells), ~10px at this cell size.
const SPINNER_CELL: f32 = 2.5;
const BUSY_REASON: &str = "Unavailable while a goal change is in progress.";
const TEXT_ONLY: &str =
    "A goal is text only. Remove attachments, mentions and comments, then send it again.";

/// The Goal switch's label and geometry (see [`Composer::goal_mode_chip_widths`]).
const GOAL_CHIP_LABEL: &str = "Goal";
const GOAL_CHIP_PAD_X: f32 = 10.0;
const GOAL_CHIP_ICON: f32 = 15.0;
const GOAL_CHIP_ICON_GAP: f32 = 6.0;

#[derive(Default)]
pub(super) struct GoalUi {
    editor: Option<GoalEditor>,
    pending: Option<GoalPending>,
    task: Option<Task<()>>,
    error: Option<(String, String)>,
    /// Composer keys (a chat id, or the new-chat canvas key) in goal mode.
    mode: HashSet<String>,
    /// Move keyboard focus into a freshly opened editor on the next frame.
    focus_editor: bool,
    /// Repaints the live timer once a second while a goal is being pursued.
    ticker: Option<Task<()>>,
}

impl GoalUi {
    pub(super) fn has_work_in_flight(&self) -> bool {
        self.pending.is_some() || self.task.as_ref().is_some_and(|task| !task.is_ready())
    }

    /// Goal mode is per profile like the drafts it applies to: an engine or
    /// profile switch drops every key's.
    pub(super) fn reset_modes(&mut self) {
        self.mode.clear();
    }
}

/// One in-flight goal change. Every goal control is disabled while it exists.
struct GoalPending {
    chat_id: String,
    kind: PendingKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind {
    Save,
    Clear,
    Toggle,
    Refresh,
}

struct GoalEditor {
    chat_id: String,
    objective: Entity<ComposerInput>,
    budget: Entity<ComposerInput>,
    /// The provider values the editor opened with, restored by Undo.
    original_objective: String,
    original_budget: String,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
enum GoalAction {
    Edit,
    Save,
    Cancel,
    Undo,
    Dismiss,
    Operation(GoalOperation, PendingKind),
}

/// A goal button: icon-only (tooltip) or icon plus short text.
struct GoalControl {
    id: &'static str,
    label: SharedString,
    text: Option<&'static str>,
    glyph: &'static str,
    enabled: bool,
    spinning: bool,
    primary: bool,
    /// Why a disabled control is unavailable (default: a goal change is in progress).
    disabled_reason: Option<&'static str>,
    action: GoalAction,
}

/// What Send does with the composer text.
pub(super) enum GoalSend {
    /// An ordinary message.
    Message,
    /// Handled here (goal mode switched on, or a validation message shown).
    Consumed,
    /// Start this objective as a Codex goal.
    Goal(String),
}

pub(super) fn goal_slash_command(provider: GoalProvider) -> SlashCommand {
    match provider {
        GoalProvider::Codex => SlashCommand {
            name: GOAL_COMMAND.into(),
            description: "Send as a goal Codex keeps working on until it is done".into(),
            input_hint: Some("objective".into()),
            kind: SlashCommandKind::Command,
        },
        GoalProvider::Claude => SlashCommand {
            name: GOAL_COMMAND.into(),
            description:
                "Send as a goal Claude keeps working on until its checker confirms the condition"
                    .into(),
            input_hint: Some("condition".into()),
            kind: SlashCommandKind::Command,
        },
    }
}

/// `/goal` → `Some("")`, `/goal ship it` → `Some("ship it")`, else `None`.
fn goal_command_objective(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix('/')?.strip_prefix(GOAL_COMMAND)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

fn grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn token_usage(goal: &ThreadGoal) -> String {
    match goal.token_budget {
        Some(budget) => format!("{} / {} tokens", grouped(goal.tokens_used), grouped(budget)),
        None => format!("{} tokens used", grouped(goal.tokens_used)),
    }
}

/// "Updated just now" from Codex's `updatedAt` (unix seconds).
fn updated_ago(updated_at: i64, now_ms: i64) -> String {
    let seconds = (now_ms / 1000 - updated_at).max(0);
    match seconds {
        0..=44 => "Updated just now".into(),
        45..=3_599 => format!("Updated {}m ago", (seconds / 60).max(1)),
        3_600..=86_399 => format!("Updated {}h ago", seconds / 3_600),
        _ => format!("Updated {}d ago", seconds / 86_400),
    }
}

/// The bar's leading words, Codex style.
fn status_title(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "Pursuing goal",
        GoalStatus::Paused => "Goal paused",
        GoalStatus::Blocked => "Goal blocked",
        GoalStatus::UsageLimited => "Goal paused at usage limit",
        GoalStatus::BudgetLimited => "Goal budget reached",
        GoalStatus::Complete => "Goal achieved",
    }
}

fn status_tone(theme: &Theme, status: GoalStatus) -> gpui::Hsla {
    match status {
        GoalStatus::Active => theme.accent,
        GoalStatus::Paused => theme.text_muted,
        GoalStatus::Complete => theme.success,
        GoalStatus::Blocked | GoalStatus::UsageLimited | GoalStatus::BudgetLimited => theme.warning,
    }
}

/// Codex's live goal clock: accounted time plus the running turn since the
/// last provider observation (TUI `GoalStatusState::indicator`). It advances
/// only while the goal is active and the chat is working.
fn live_seconds(goal: &ThreadGoal, checked_at_ms: i64, working: bool, now_ms: i64) -> i64 {
    if goal.status == GoalStatus::Active && working {
        goal.time_used_seconds + (now_ms - checked_at_ms).max(0) / 1000
    } else {
        goal.time_used_seconds
    }
}

/// Claude's own scheduled prompts. Claude fires them between turns while its
/// session runs; LUNA only shows them, with their real lifetime.
fn render_schedules(schedules: &[NativeSchedule], below_goal: bool, theme: &Theme) -> AnyElement {
    const SHOWN: usize = 4;
    let count = schedules.len();
    let title = if count == 1 {
        "1 prompt scheduled by Claude".to_owned()
    } else {
        format!("{count} prompts scheduled by Claude")
    };
    let rows = schedules
        .iter()
        .take(SHOWN)
        .enumerate()
        .map(|(index, schedule)| {
            let kind = match (schedule.kind, schedule.recurring) {
                (NativeScheduleKind::Wakeup, _) => "loop wakeup",
                (_, true) => "recurring",
                (_, false) => "one-shot",
            };
            let lifetime = if schedule.durable {
                "saved in project"
            } else {
                "session-only"
            };
            div()
                .id(("goal-schedule", index))
                .role(Role::ListItem)
                .aria_label(format!(
                    "{}: {} ({kind}, {lifetime})",
                    schedule.schedule, schedule.prompt
                ))
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(7.0))
                .text_size(crate::typography::ui_rems(12.0))
                .child(
                    crate::icons::icon(crate::icons::CLOCK_CIRCLE)
                        .size(px(13.0))
                        .text_color(theme.readable_muted(0.7)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.text)
                        .child(schedule.schedule.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.readable_muted(0.78))
                        .child(schedule.prompt.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.readable_faint(0.85))
                        .child(format!("{kind} \u{00b7} {lifetime}")),
                )
        });
    div()
        .id("goal-schedules")
        .role(Role::Group)
        .aria_label(title.clone())
        .aria_description(CLAUDE_SCHEDULE_LIFETIME)
        .mx(px(12.0))
        .mb(px(10.0))
        .when(below_goal, |el| {
            el.pt(px(8.0))
                .border_t_1()
                .border_color(theme.border.opacity(0.7))
        })
        .when(!below_goal, |el| el.pt(px(10.0)))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .pl(px(2.0))
                .text_size(crate::typography::ui_rems(12.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(title),
        )
        .child(
            div()
                .id("goal-schedule-list")
                .role(Role::List)
                .pl(px(2.0))
                .flex()
                .flex_col()
                .gap(px(5.0))
                .children(rows),
        )
        .when(count > SHOWN, |el| {
            el.child(
                div()
                    .pl(px(22.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.readable_muted(0.72))
                    .child(format!("+{} more", count - SHOWN)),
            )
        })
        .child(
            div()
                .pl(px(2.0))
                .text_size(crate::typography::ui_rems(11.0))
                .line_height(crate::typography::ui_rems(15.0))
                .text_color(theme.readable_faint(0.9))
                .child(CLAUDE_SCHEDULE_LIFETIME),
        )
        .into_any_element()
}

impl Composer {
    /// The selected chat's goal provider (Codex or Claude Code).
    fn chat_goal_provider(&self, cx: &App) -> Option<GoalProvider> {
        self.binding
            .chat_row(self.state.read(cx))
            .and_then(|chat| chat.config.as_ref())
            .and_then(|config| GoalProvider::of(config.harness))
    }

    /// The goal provider for this composer: the chat's, or the new-chat picker's.
    pub(super) fn goal_provider(&self, cx: &App) -> Option<GoalProvider> {
        if self.binding.chat_id(self.state.read(cx)).is_some() {
            return self.chat_goal_provider(cx);
        }
        self.pickers
            .read(cx)
            .resolved(cx)
            .harness
            .and_then(GoalProvider::of)
    }

    /// `/goal` is offered on existing Codex and Claude Code chats.
    pub(super) fn goal_command_available(&self, cx: &App) -> bool {
        self.chat_goal_provider(cx).is_some()
    }

    /// Goal mode needs Codex or Claude Code: the chat's provider, or the
    /// new-chat picker's.
    pub(super) fn goal_mode_available(&self, cx: &App) -> bool {
        self.editing_queued.is_none() && self.goal_provider(cx).is_some()
    }

    pub(super) fn goal_mode_on(&self, cx: &App) -> bool {
        self.goal_ui.mode.contains(&self.current_key) && self.goal_mode_available(cx)
    }

    fn set_goal_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        let key = self.current_key.clone();
        if on {
            self.goal_ui.mode.insert(key);
        } else {
            self.goal_ui.mode.remove(&key);
        }
        self.focus_pending = true;
        cx.notify();
    }

    /// The provider's goal placeholder while goal mode is on.
    pub(super) fn sync_goal_placeholder(&mut self, cx: &mut Context<Self>) {
        let wanted = match self.goal_provider(cx) {
            Some(provider) if self.goal_mode_on(cx) => provider.placeholder(),
            _ => MESSAGE_PLACEHOLDER,
        };
        if self.input.read(cx).placeholder.as_ref() != wanted {
            self.input
                .update(cx, |input, cx| input.set_placeholder(wanted, cx));
        }
    }

    /// Decide whether Send starts a goal. `/goal <objective>` always does on a
    /// Codex chat; plain text does while goal mode is on. Bare `/goal` turns
    /// goal mode on, like Codex's command.
    pub(super) fn goal_send(&mut self, text: &str, cx: &mut Context<Self>) -> GoalSend {
        let command = goal_command_objective(text);
        if command.is_none() && !self.goal_mode_on(cx) {
            return GoalSend::Message;
        }
        if !self.goal_mode_available(cx) {
            return GoalSend::Message;
        }
        if command == Some("") {
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.set_goal_mode(true, cx);
            return GoalSend::Consumed;
        }
        let objective = command.unwrap_or(text).trim().to_owned();
        if objective.is_empty() {
            return GoalSend::Consumed;
        }
        if self.goal_ui.pending.is_some() {
            self.failure = Some("Wait for the current goal change to finish.".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return GoalSend::Consumed;
        }
        let staged = self.staged().len()
            + self.staged_appshots().len()
            + self.input_items.get(&self.current_key).map_or(0, Vec::len)
            + self.staged_comments(cx).len();
        let limit = (objective.chars().count() > 4_000)
            .then_some("A goal objective must be at most 4000 characters.");
        if let Some(reason) = (staged > 0).then_some(TEXT_ONLY).or(limit) {
            self.failure = Some(reason.into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return GoalSend::Consumed;
        }
        // Goal mode is NOT switched on here: a `/goal <objective>` that fails
        // a preflight check keeps its `/goal` text, and a mode left on would
        // turn the next ordinary message into a goal. Only a deferred send or
        // a rejected draft handed back re-arms it (`restore_goal_mode`).
        if let Some(chat) = self
            .binding
            .chat_id(self.state.read(cx))
            .map(str::to_string)
            && self
                .goal_ui
                .error
                .as_ref()
                .is_some_and(|(owner, _)| *owner == chat)
        {
            self.goal_ui.error = None;
        }
        GoalSend::Goal(objective)
    }

    pub(super) fn goal_handed_off(&mut self) {
        self.goal_ui.mode.remove(&self.current_key);
    }

    /// A rejected goal send hands its draft back in goal mode.
    pub(super) fn restore_goal_mode(&mut self, key: &str) {
        self.goal_ui.mode.insert(key.to_owned());
    }

    fn current_goal(&self, cx: &App) -> Option<ThreadGoal> {
        self.binding
            .context_control(self.state.read(cx))
            .goal
            .as_ref()
            .and_then(|readback| readback.goal.clone())
    }

    fn open_goal_editor(&mut self, cx: &mut Context<Self>) {
        if self.goal_ui.pending.is_some() {
            return;
        }
        let Some(chat_id) = self
            .binding
            .chat_id(self.state.read(cx))
            .map(str::to_string)
        else {
            return;
        };
        let Some(goal) = self.current_goal(cx) else {
            return;
        };
        // Claude has no in-place goal edit (a new `/goal` replaces it).
        if self.chat_goal_provider(cx) != Some(GoalProvider::Codex) {
            return;
        }
        let objective_text = goal.objective.clone();
        let budget_text = goal
            .token_budget
            .map(|tokens| tokens.to_string())
            .unwrap_or_default();
        let objective = cx.new(|cx| {
            let mut input = ComposerInput::new("What should Codex keep working toward?", cx)
                .with_text_metrics(12.5, 18.0);
            input.set_text(objective_text.clone(), cx);
            input
        });
        let budget = cx.new(|cx| {
            let mut input = ComposerInput::new("No limit", cx)
                .with_single_line()
                .with_text_metrics(12.5, 18.0);
            input.set_text(budget_text.clone(), cx);
            input
        });
        let mut subscriptions = vec![
            cx.observe(&objective, |_: &mut Self, _, cx| cx.notify()),
            cx.observe(&budget, |_: &mut Self, _, cx| cx.notify()),
        ];
        for input in [&objective, &budget] {
            subscriptions.push(
                cx.subscribe(input, |this: &mut Self, _, event, cx| match event {
                    ComposerInputEvent::Submitted
                    | ComposerInputEvent::ModifierSubmitted
                    | ComposerInputEvent::OppositeSubmitted => this.save_goal_editor(cx),
                    _ => cx.notify(),
                }),
            );
        }
        if self
            .goal_ui
            .error
            .as_ref()
            .is_some_and(|(owner, _)| owner == &chat_id)
        {
            self.goal_ui.error = None;
        }
        self.goal_ui.editor = Some(GoalEditor {
            chat_id,
            objective,
            budget,
            original_objective: objective_text,
            original_budget: budget_text,
            _subscriptions: subscriptions,
        });
        self.goal_ui.focus_editor = true;
        cx.notify();
    }

    /// Cancel: drop the editor and any validation message it produced.
    fn close_goal_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.goal_ui.editor.take() {
            if self
                .goal_ui
                .error
                .as_ref()
                .is_some_and(|(owner, _)| *owner == editor.chat_id)
            {
                self.goal_ui.error = None;
            }
            self.focus_pending = true;
        }
        cx.notify();
    }

    fn goal_editor_dirty(&self, cx: &App) -> bool {
        self.goal_ui.editor.as_ref().is_some_and(|editor| {
            editor.objective.read(cx).text() != editor.original_objective
                || editor.budget.read(cx).text() != editor.original_budget
        })
    }

    /// Undo: put back what Codex last reported.
    fn undo_goal_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.goal_ui.editor.as_ref() else {
            return;
        };
        let (objective, budget) = (editor.objective.clone(), editor.budget.clone());
        let (objective_text, budget_text) = (
            editor.original_objective.clone(),
            editor.original_budget.clone(),
        );
        objective.update(cx, |input, cx| input.set_text(objective_text, cx));
        budget.update(cx, |input, cx| input.set_text(budget_text, cx));
        self.goal_ui.focus_editor = true;
        cx.notify();
    }

    fn save_goal_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.goal_ui.editor.as_ref() else {
            return;
        };
        if self.goal_ui.pending.is_some()
            || self.binding.chat_id(self.state.read(cx)) != Some(editor.chat_id.as_str())
        {
            return;
        }
        let objective = editor.objective.read(cx).text().trim().to_owned();
        let budget = editor.budget.read(cx).text().trim().to_owned();
        let token_budget = if budget.is_empty() {
            GoalBudget::Unlimited
        } else {
            match budget.replace([',', '_'], "").parse::<i64>() {
                Ok(tokens) if tokens > 0 => GoalBudget::Limited { tokens },
                _ => {
                    self.goal_ui.error = Some((
                        editor.chat_id.clone(),
                        "Enter a positive token budget, or leave it blank for no limit.".into(),
                    ));
                    cx.notify();
                    return;
                }
            }
        };
        // Codex steers a running goal turn with a changed objective.
        let operation = GoalOperation::Set {
            objective: Some(objective),
            status: None,
            token_budget: Some(token_budget),
        };
        self.submit_goal_operation(operation, PendingKind::Save, cx);
    }

    /// Run the live timer while a goal is pursued; stop it otherwise.
    fn drive_goal_timer(&mut self, live: bool, cx: &mut Context<Self>) {
        if !live {
            self.goal_ui.ticker = None;
            return;
        }
        if self.goal_ui.ticker.is_none() {
            self.goal_ui.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    fn submit_goal_operation(
        &mut self,
        operation: GoalOperation,
        kind: PendingKind,
        cx: &mut Context<Self>,
    ) {
        if self.goal_ui.pending.is_some() {
            return;
        }
        let state = self.state.read(cx);
        let Some(chat_id) = self.binding.chat_id(state).map(str::to_string) else {
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            self.goal_ui.error = Some((
                chat_id,
                "LUNA is not connected. Reconnect, then try the goal change again.".into(),
            ));
            cx.notify();
            return;
        };
        if let Err(error) = operation.validate() {
            self.goal_ui.error = Some((chat_id, error.into()));
            cx.notify();
            return;
        }
        self.goal_ui.pending = Some(GoalPending {
            chat_id: chat_id.clone(),
            kind,
        });
        self.goal_ui.error = None;
        self.goal_ui.task = Some(cx.spawn(async move |this, cx| {
            let intent = serde_json::json!({"kind": "goal", "operation": operation});
            let result = crate::task_commands::execute(&engine, &chat_id, intent).await;
            this.update(cx, |composer, cx| {
                composer.goal_ui.pending = None;
                match result {
                    Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                        if composer
                            .goal_ui
                            .editor
                            .as_ref()
                            .is_some_and(|editor| editor.chat_id == chat_id)
                        {
                            composer.goal_ui.editor = None;
                            composer.focus_pending = composer.current_key == chat_id;
                        }
                    }
                    Ok(_) => composer.goal_ui.error = Some((
                        chat_id.clone(),
                        "The goal change is pending or unconfirmed. Refresh before another change."
                            .into(),
                    )),
                    Err(error) => {
                        composer.goal_ui.error = Some((
                            chat_id.clone(),
                            format!("The goal change failed or is unconfirmed: {error}"),
                        ))
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn goal_control(
        &self,
        control: GoalControl,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let GoalControl {
            id,
            label,
            text,
            glyph,
            enabled,
            spinning,
            primary,
            disabled_reason,
            action,
        } = control;
        let owner = self
            .binding
            .chat_id(self.state.read(cx))
            .map(str::to_string);
        let icon_only = text.is_none();
        let group = SharedString::from(format!("{id}-group"));
        let (rest, hover) = if primary {
            (theme.on_accent, theme.on_accent)
        } else {
            (theme.readable_muted(0.85), theme.text)
        };
        let accent = theme.accent;
        let accent_strong = theme.accent_strong;
        let wash = theme.ink(0.08);
        let glyph = if spinning {
            div()
                .size(px(GLYPH))
                .flex()
                .items_center()
                .justify_center()
                .child(crate::loaders::gradient_spinner(
                    "goal-control-spinner",
                    theme,
                    SPINNER_CELL,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element()
        } else {
            crate::icons::icon(glyph)
                .size(px(if icon_only { GLYPH } else { 12.0 }))
                .text_color(rest)
                .when(enabled, |icon| {
                    icon.group_hover(group.clone(), move |style| style.text_color(hover))
                })
                .into_any_element()
        };
        let tooltip = label.clone();
        gpui_base::Button::new(id)
            .role(Role::Button)
            .accessibility_id(id)
            .accessibility_label(label)
            .disabled(!enabled)
            .tab_index(0)
            .tab_stop(enabled)
            .group(group)
            .h(px(24.0))
            .when(icon_only, |button| button.w(px(24.0)))
            .when(!icon_only, |button| button.px(px(8.0)))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(5.0))
            .rounded(px(6.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(rest)
            .when(primary, |button| {
                button.bg(accent).font_weight(gpui::FontWeight::MEDIUM)
            })
            .when(enabled && primary, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(accent_strong))
                    .focus_visible(move |style| style.bg(accent_strong))
            })
            .when(enabled && !primary, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(wash).text_color(hover))
                    .focus_visible(move |style| style.bg(accent.opacity(0.18)).text_color(accent))
            })
            // The control that is working keeps full strength for its spinner.
            .when(!enabled, |button| {
                button
                    .when(!spinning, |button| button.opacity(0.45))
                    .aria_description(if spinning {
                        "In progress."
                    } else {
                        disabled_reason.unwrap_or(BUSY_REASON)
                    })
            })
            .when(icon_only, |button| {
                button
                    .tooltip(crate::popover::text_tooltip(tooltip.clone()))
                    .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.binding.chat_id(this.state.read(cx)) != owner.as_deref() {
                    return;
                }
                if this.goal_ui.pending.is_some() {
                    return;
                }
                match &action {
                    GoalAction::Edit => this.open_goal_editor(cx),
                    GoalAction::Save => this.save_goal_editor(cx),
                    GoalAction::Cancel => this.close_goal_editor(cx),
                    GoalAction::Undo => this.undo_goal_editor(cx),
                    GoalAction::Dismiss => {
                        this.goal_ui.error = None;
                        cx.notify();
                    }
                    GoalAction::Operation(operation, kind) => {
                        this.submit_goal_operation(operation.clone(), *kind, cx)
                    }
                }
            }))
            .child(glyph)
            .children(text)
            .into_any_element()
    }

    /// The Goal badge's `(label, icon-only)` widths for the controls row's
    /// fit, or `None` while goal mode is off.
    pub(super) fn goal_mode_chip_widths(&self, window: &Window, cx: &App) -> Option<(f32, f32)> {
        if !self.goal_mode_on(cx) || self.goal_provider(cx).is_none() {
            return None;
        }
        let label = crate::pickers::chip_text_width(GOAL_CHIP_LABEL, window, Theme::of(cx));
        Some((
            2.0 * GOAL_CHIP_PAD_X + GOAL_CHIP_ICON + GOAL_CHIP_ICON_GAP + label,
            crate::pickers::ACCESS_CHIP_SIZE,
        ))
    }

    /// The composer's "Goal" badge, beside the access chip while goal mode is
    /// on — that is, only after this composer's own `/goal`. It never stands
    /// for a goal the chat is running (the goal bar does). Clicking it turns
    /// goal mode off. `icon_only` drops the label when the controls row is
    /// short of room; the tooltip then names the badge and its state.
    pub(super) fn render_goal_mode_chip(
        &self,
        icon_only: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.goal_mode_on(cx) {
            return None;
        }
        let provider = self.goal_provider(cx)?;
        let theme = Theme::of(cx);
        let accent = theme.accent;
        let rest = theme.readable_tone(accent, 0.95);
        let owner = self.current_key.clone();
        Some(
            gpui_base::Button::new("goal-mode")
                .role(Role::Switch)
                .accessibility_id("goal-mode")
                .accessibility_label("Goal mode")
                .aria_toggled(gpui::Toggled::True)
                .aria_description(provider.mode_on_description())
                .tab_index(0)
                .flex_none()
                .h(px(crate::pickers::CONTROL_HEIGHT))
                .when(icon_only, |chip| {
                    chip.w(px(crate::pickers::ACCESS_CHIP_SIZE))
                        .justify_center()
                })
                .when(!icon_only, |chip| chip.px(px(GOAL_CHIP_PAD_X)))
                .flex()
                .items_center()
                .gap(px(GOAL_CHIP_ICON_GAP))
                .rounded_full()
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rest)
                .bg(accent.opacity(0.14))
                .cursor_pointer()
                .hover(move |style| style.bg(accent.opacity(0.2)).text_color(rest))
                .focus_visible({
                    let ring = theme.focus_ring();
                    move |style| crate::theme::focus_ring(style, ring)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.current_key == owner {
                        this.set_goal_mode(false, cx);
                    }
                }))
                .tooltip_above(move |_, cx| {
                    crate::pickers::ControlTooltip::view(
                        "Goal mode is on",
                        Some(
                            format!("{} Click to turn it off.", provider.mode_on_description())
                                .into(),
                        ),
                        cx,
                    )
                })
                .child(
                    crate::icons::icon(crate::icons::TARGET)
                        .size(px(GOAL_CHIP_ICON))
                        .text_color(rest),
                )
                .when(!icon_only, |chip| chip.child(GOAL_CHIP_LABEL))
                .into_any_element(),
        )
    }

    /// The goal bar, or `None` when this chat has no unfinished goal (Codex
    /// removes it once the goal is achieved). Placement belongs to the caller.
    pub(super) fn render_goal_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let selected = {
            let state = self.state.read(cx);
            let control = self.binding.context_control(state);
            self.binding.chat_row(state).and_then(|chat| {
                let provider = chat
                    .config
                    .as_ref()
                    .and_then(|config| GoalProvider::of(config.harness))?;
                let working = matches!(
                    state.indicator_for(&chat.id, chrono::Utc::now()),
                    crate::state::Indicator::Working
                );
                // Claude's own schedules that it still reports live.
                let schedules: Vec<NativeSchedule> = if provider == GoalProvider::Claude {
                    control
                        .native_schedules
                        .iter()
                        .filter(|schedule| schedule.is_live(now_ms))
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                Some((
                    chat.id.clone(),
                    provider,
                    control.goal.clone(),
                    working,
                    schedules,
                ))
            })
        };
        let Some((chat_id, provider, readback, working, schedules)) = selected else {
            self.drive_goal_timer(false, cx);
            return None;
        };
        let checked_at_ms = readback
            .as_ref()
            .map_or(0, |readback| readback.checked_at_ms);
        let goal = readback
            .and_then(|readback| readback.goal)
            .filter(|goal| goal.status != GoalStatus::Complete);
        let editor = self
            .goal_ui
            .editor
            .as_ref()
            .filter(|editor| editor.chat_id == chat_id)
            .map(|editor| (editor.objective.clone(), editor.budget.clone()));
        let error = self
            .goal_ui
            .error
            .as_ref()
            .filter(|(owner, _)| owner == &chat_id)
            .map(|(_, error)| error.clone());
        let busy = self.goal_ui.pending.is_some();
        let pending = self
            .goal_ui
            .pending
            .as_ref()
            .filter(|pending| pending.chat_id == chat_id)
            .map(|pending| pending.kind);
        if goal.is_none() && error.is_none() && pending.is_none() && schedules.is_empty() {
            self.drive_goal_timer(false, cx);
            return None;
        }
        let live = goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Active && working);
        let needs_runtime = provider == GoalProvider::Codex
            && !working
            && goal
                .as_ref()
                .is_some_and(|goal| goal.status == GoalStatus::Active)
            && self
                .binding
                .context_control(self.state.read(cx))
                .goal
                .as_ref()
                .is_some_and(|readback| !readback.continuation_ready);
        self.drive_goal_timer(live, cx);
        if std::mem::take(&mut self.goal_ui.focus_editor)
            && let Some((objective, _)) = &editor
        {
            let focus = objective.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
        }
        let theme = Theme::of(cx).clone();
        let editing = editor.is_some() && goal.is_some();

        // ---- the bar: target · title · objective · timer | controls ----
        let status = goal.as_ref().map(|goal| goal.status);
        let tone = status.map_or(theme.accent, |status| status_tone(&theme, status));
        let title = if needs_runtime {
            "Goal not running"
        } else {
            status.map_or("Goal", status_title)
        };
        let objective_preview = goal.as_ref().map(|goal| {
            goal.objective
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
        let timer = goal.as_ref().map(|goal| {
            crate::transcript::format_elapsed(live_seconds(goal, checked_at_ms, working, now_ms))
        });
        let timer_label = timer.clone().map(|timer| {
            if live {
                format!("{timer} so far")
            } else {
                format!("{timer} worked")
            }
        });
        // Claude's checker runs after each turn; count its verdicts.
        let checks = goal
            .as_ref()
            .and_then(|goal| goal.evaluation.as_ref())
            .map(|evaluation| evaluation.checks)
            .filter(|checks| *checks > 0)
            .map(|checks| {
                if checks == 1 {
                    "1 check".to_owned()
                } else {
                    format!("{checks} checks")
                }
            });
        let summary = div()
            .id("goal-summary")
            .role(Role::Status)
            .aria_label(
                [
                    Some(title.to_owned()),
                    objective_preview.clone(),
                    timer_label,
                    checks.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
            )
            .flex_1()
            .min_w_0()
            .h_full()
            .pl(px(14.0))
            .pr(px(6.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .child(
                crate::icons::icon(crate::icons::TARGET)
                    .size(px(GLYPH))
                    .text_color(tone),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
            )
            // The one separator sits between the title and the objective;
            // trailing metadata needs none (audit CP-22).
            .when(objective_preview.is_some(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.readable_faint(0.8))
                        .child("·"),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.readable_muted(0.8))
                    .children(objective_preview),
            )
            .when_some(timer, |el, timer| {
                el.child(
                    div()
                        .id("goal-timer")
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .font_features(crate::typography::tabular_figures())
                        .text_color(if live {
                            theme.text.opacity(0.85)
                        } else {
                            theme.readable_muted(0.75)
                        })
                        .child(timer),
                )
            })
            .when_some(checks, |el, checks| {
                el.child(
                    div()
                        .id("goal-checks")
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.readable_muted(0.75))
                        .child(div().text_color(theme.readable_faint(0.8)).child("·"))
                        .child(checks),
                )
            });
        let claude = provider == GoalProvider::Claude;
        let mut controls: Vec<AnyElement> = Vec::new();
        if let Some(goal) = goal.as_ref() {
            // Claude has no in-place edit: a new `/goal` replaces the goal
            // and starts work at once, so it is a send, not an edit.
            if !claude {
                controls.push(self.goal_control(
                    GoalControl {
                        id: "goal-edit",
                        label: "Edit goal".into(),
                        text: None,
                        glyph: crate::icons::PEN,
                        enabled: !busy && !editing,
                        spinning: false,
                        primary: false,
                        disabled_reason: None,
                        action: GoalAction::Edit,
                    },
                    &theme,
                    cx,
                ));
            }
            // Pause keeps the running turn (Codex semantics). A reached budget
            // can resume after Edit raises or removes the exhausted limit.
            // Claude goals cannot be paused at all.
            let toggle = match goal.status {
                _ if claude => None,
                GoalStatus::Active if needs_runtime => {
                    Some(("Resume goal", crate::icons::PLAY, GoalStatus::Active))
                }
                GoalStatus::Active => Some(("Pause goal", crate::icons::PAUSE, GoalStatus::Paused)),
                GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::UsageLimited => {
                    Some(("Resume goal", crate::icons::PLAY, GoalStatus::Active))
                }
                GoalStatus::BudgetLimited
                    if goal
                        .token_budget
                        .is_none_or(|budget| budget > goal.tokens_used) =>
                {
                    Some(("Resume goal", crate::icons::PLAY, GoalStatus::Active))
                }
                GoalStatus::BudgetLimited | GoalStatus::Complete => None,
            };
            if let Some((label, glyph, next)) = toggle {
                controls.push(self.goal_control(
                    GoalControl {
                        id: "goal-toggle",
                        label: label.into(),
                        text: None,
                        glyph,
                        enabled: !busy,
                        spinning: pending == Some(PendingKind::Toggle),
                        primary: false,
                        disabled_reason: None,
                        action: GoalAction::Operation(
                            GoalOperation::Set {
                                objective: None,
                                status: Some(next),
                                token_budget: None,
                            },
                            PendingKind::Toggle,
                        ),
                    },
                    &theme,
                    cx,
                ));
            }
            // Claude clears a goal between turns (its own `/goal clear`).
            let between_turns = claude && working;
            controls.push(self.goal_control(
                GoalControl {
                    id: "goal-clear",
                    label: if between_turns {
                        CLAUDE_CLEAR_BETWEEN_TURNS.into()
                    } else {
                        "Clear goal".into()
                    },
                    text: None,
                    glyph: crate::icons::TRASH_BIN_MINIMALISTIC,
                    enabled: !busy && !between_turns,
                    spinning: pending == Some(PendingKind::Clear),
                    primary: false,
                    disabled_reason: between_turns.then_some(CLAUDE_CLEAR_BETWEEN_TURNS),
                    action: GoalAction::Operation(GoalOperation::Clear, PendingKind::Clear),
                },
                &theme,
                cx,
            ));
        } else if pending.is_some() {
            controls.push(
                div()
                    .id("goal-progress")
                    .role(Role::ProgressIndicator)
                    .aria_label("Goal change in progress")
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::loaders::gradient_spinner(
                        "goal-change-spinner",
                        &theme,
                        SPINNER_CELL,
                        cx.entity_id(),
                        cx,
                    ))
                    .into_any_element(),
            );
        } else {
            controls.push(self.goal_control(
                GoalControl {
                    id: "goal-dismiss",
                    label: "Dismiss goal message".into(),
                    text: None,
                    glyph: crate::icons::CLOSE,
                    enabled: !busy,
                    spinning: false,
                    primary: false,
                    disabled_reason: None,
                    action: GoalAction::Dismiss,
                },
                &theme,
                cx,
            ));
        }
        let header = div()
            .id("goal-bar-header")
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .child(summary)
            .child(
                div()
                    .flex_none()
                    .h_full()
                    .pr(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .children(controls),
            );

        let body = match (editor, goal.as_ref()) {
            (Some((objective, budget)), Some(goal)) => {
                Some(self.render_goal_editor(objective, budget, goal, pending, now_ms, &theme, cx))
            }
            _ => None,
        };
        let show_header = goal.is_some() || pending.is_some() || error.is_some();
        // Claude: the checker's latest reason, and that headless Claude
        // checks again only after the next message.
        let checker_line = goal
            .as_ref()
            .filter(|goal| claude && goal.status == GoalStatus::Active)
            .and_then(|goal| {
                let reason = goal
                    .evaluation
                    .as_ref()
                    .and_then(|evaluation| evaluation.last_reason.clone())
                    .map(|reason| format!("Last check: {reason}"));
                let waiting = (!working).then_some("Claude checks again after your next message.");
                let text = match (reason, waiting) {
                    (Some(reason), Some(waiting)) => format!("{reason} \u{00b7} {waiting}"),
                    (Some(reason), None) => reason,
                    (None, Some(waiting)) => waiting.to_owned(),
                    (None, None) => return None,
                };
                Some(
                    div()
                        .id("goal-checker")
                        .role(Role::Status)
                        .aria_label(text.clone())
                        .mx(px(14.0))
                        .mt(px(-6.0))
                        .mb(px(8.0))
                        .pl(px(21.0))
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.readable_muted(0.72))
                        .child(text)
                        .into_any_element(),
                )
            });
        let schedules_section =
            (!schedules.is_empty()).then(|| render_schedules(&schedules, show_header, &theme));
        let error = error.map(|error| {
            let refresh = self.goal_control(
                GoalControl {
                    id: "goal-refresh",
                    label: "Refresh goal status".into(),
                    text: None,
                    glyph: crate::icons::REFRESH,
                    enabled: !busy,
                    spinning: pending == Some(PendingKind::Refresh),
                    primary: false,
                    disabled_reason: None,
                    action: GoalAction::Operation(GoalOperation::Get, PendingKind::Refresh),
                },
                &theme,
                cx,
            );
            div()
                .id("goal-error")
                .role(Role::Alert)
                .mx(px(12.0))
                .mb(px(10.0))
                .pl(px(10.0))
                .pr(px(4.0))
                .py(px(4.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.danger.opacity(0.22))
                .bg(theme.danger.opacity(0.07))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(11.5))
                .line_height(crate::typography::ui_rems(16.0))
                .text_color(theme.readable_tone(theme.danger_muted, 0.92))
                .child(div().flex_1().min_w_0().py(px(3.0)).child(error))
                .child(refresh)
        });

        let tray = div()
            .id("goal-card")
            .role(Role::Group)
            .aria_label(if show_header {
                "Goal"
            } else {
                "Claude schedules"
            })
            .occlude()
            .rounded_t(px(CARD_RADIUS))
            .bg(crate::popover::surface_bg(&theme))
            .border_1()
            .border_color(theme.border)
            .when(!theme.is_frost(), |el| {
                el.shadow(crate::theme::menu_shadows())
            })
            // Only the strip tucked behind the next tray needs padding.
            .pb(px(QUEUE_COMPOSER_OVERLAP))
            .flex()
            .flex_col()
            .text_color(theme.text)
            .when(show_header, |el| el.child(header))
            .children(checker_line)
            .children(body)
            .children(error)
            .children(schedules_section);
        Some(crate::frost::frosted(CARD_RADIUS, crate::frost::MENU_BLUR, tray).into_any_element())
    }

    /// Codex's "Edit goal" surface, inline: objective, budget, "Updated …",
    /// Undo and Save. It stays bound to the chat that opened it.
    #[allow(clippy::too_many_arguments)]
    fn render_goal_editor(
        &self,
        objective: Entity<ComposerInput>,
        budget: Entity<ComposerInput>,
        goal: &ThreadGoal,
        pending: Option<PendingKind>,
        now_ms: i64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.goal_ui.pending.is_some();
        let dirty = self.goal_editor_dirty(cx);
        let field = |label: &'static str, input: Entity<ComposerInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.readable_muted(0.8))
                        .child(label),
                )
                .child(
                    div()
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.ink(0.03))
                        .child(input),
                )
        };
        let undo = self.goal_control(
            GoalControl {
                id: "goal-undo",
                label: "Undo goal edits".into(),
                text: Some("Undo"),
                glyph: crate::icons::RESTART,
                enabled: !busy && dirty,
                spinning: false,
                primary: false,
                disabled_reason: None,
                action: GoalAction::Undo,
            },
            theme,
            cx,
        );
        let cancel = self.goal_control(
            GoalControl {
                id: "goal-cancel",
                label: "Close goal editor".into(),
                text: Some("Cancel"),
                glyph: crate::icons::CLOSE,
                enabled: !busy,
                spinning: false,
                primary: false,
                disabled_reason: None,
                action: GoalAction::Cancel,
            },
            theme,
            cx,
        );
        let save = self.goal_control(
            GoalControl {
                id: "goal-save",
                label: "Save goal".into(),
                text: Some("Save"),
                glyph: crate::icons::CHECK,
                enabled: !busy,
                spinning: pending == Some(PendingKind::Save),
                primary: true,
                disabled_reason: None,
                action: GoalAction::Save,
            },
            theme,
            cx,
        );
        div()
            .id("goal-editor")
            .role(Role::Group)
            .aria_label("Edit goal")
            .border_t_1()
            .border_color(theme.border.opacity(0.6))
            .px(px(14.0))
            .pt(px(10.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.goal_ui.pending.is_none() {
                    cx.stop_propagation();
                    this.close_goal_editor(cx);
                }
            }))
            .child(field("Objective", objective))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(10.0))
                    .child(field("Token budget", budget).w(px(180.0)))
                    .child(
                        div()
                            .pb(px(8.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_faint(0.9))
                            .child("Optional. Leave blank for no limit."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("goal-updated")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_faint(0.9))
                            .child(format!(
                                "{} · {}",
                                updated_ago(goal.updated_at, now_ms),
                                token_usage(goal)
                            )),
                    )
                    .child(undo)
                    .child(cancel)
                    .child(save),
            )
            .into_any_element()
    }
}
