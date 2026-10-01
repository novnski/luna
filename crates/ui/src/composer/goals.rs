//! The goal card: a compact tray stacked on top of the composer while a Codex
//! goal exists (adapted from BB's ThreadGoalCard). Nothing renders without a
//! goal; `/goal [objective]` creates or edits one. Edits are bound to the chat
//! that opened the editor, every control re-checks its owning chat, and
//! provider snapshots drive every status and usage label.
use super::*;
use gpui::AnyElement;
use luna_proto::{GoalBudget, GoalOperation, GoalStatus, ThreadGoal};

#[cfg(feature = "appshots-fixture")]
#[path = "goals_e2e.rs"]
mod e2e;
#[cfg(feature = "appshots-fixture")]
pub use e2e::run_goal_card_e2e;

/// LUNA's own slash command, offered on existing Codex chats only.
pub(super) const GOAL_COMMAND: &str = "goal";
/// The queue tray's corner, so stacked trays read as one family.
const CARD_RADIUS: f32 = 16.0;
const HEADER_HEIGHT: f32 = 36.0;
const GLYPH: f32 = 14.0;
const META_GLYPH: f32 = 12.0;
const BUDGET_METER_WIDTH: f32 = 56.0;
const BUSY_REASON: &str = "Unavailable while a goal change is in progress.";

#[derive(Default)]
pub(super) struct GoalUi {
    editor: Option<GoalEditor>,
    pending: Option<GoalPending>,
    task: Option<Task<()>>,
    error: Option<(String, String)>,
    /// Chats whose card body is open. Cards start collapsed.
    expanded: HashSet<String>,
    /// Move keyboard focus into a freshly opened editor on the next frame.
    focus_editor: bool,
}

/// One in-flight goal change. Every goal control is disabled while it exists.
struct GoalPending {
    chat_id: String,
    kind: PendingKind,
    /// The `/goal …` text that issued this change. The composer clears it only
    /// once the change is applied, so a failure leaves it in place for retry.
    command_text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind {
    Save,
    Clear,
    Toggle,
    Complete,
    Refresh,
}

struct GoalEditor {
    chat_id: String,
    objective: Entity<ComposerInput>,
    budget: Entity<ComposerInput>,
    creating: bool,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone)]
enum GoalAction {
    Edit,
    Save,
    Cancel,
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
    action: GoalAction,
}

pub(super) fn goal_slash_command() -> SlashCommand {
    SlashCommand {
        name: GOAL_COMMAND.into(),
        description: "Set a goal Codex keeps working toward".into(),
        input_hint: Some("objective".into()),
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
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn token_usage(goal: &ThreadGoal) -> String {
    match goal.token_budget {
        Some(budget) => format!("{} / {} tokens", grouped(goal.tokens_used), grouped(budget)),
        None => format!("{} tokens", grouped(goal.tokens_used)),
    }
}

fn elapsed(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        let rest = seconds % 60;
        return if rest > 0 {
            format!("{minutes}m {rest}s")
        } else {
            format!("{minutes}m")
        };
    }
    let (hours, rest) = (minutes / 60, minutes % 60);
    if rest > 0 {
        format!("{hours}h {rest}m")
    } else {
        format!("{hours}h")
    }
}

/// The cached observation's age, never a wall-clock timestamp.
fn checked_ago(checked_at_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - checked_at_ms).max(0) / 1000;
    match seconds {
        0..=44 => "checked just now".into(),
        45..=3_599 => format!("checked {}m ago", (seconds / 60).max(1)),
        3_600..=86_399 => format!("checked {}h ago", seconds / 3_600),
        _ => format!("checked {}d ago", seconds / 86_400),
    }
}

fn status_chip_label(status: GoalStatus) -> Option<&'static str> {
    match status {
        GoalStatus::Active => None,
        GoalStatus::Paused => Some("Paused"),
        GoalStatus::Blocked => Some("Blocked"),
        GoalStatus::UsageLimited => Some("Usage limit reached"),
        GoalStatus::BudgetLimited => Some("Budget reached"),
        GoalStatus::Complete => Some("Complete"),
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

/// Muted readback line: how the goal continues, then the observation's age.
fn readback_note(goal: &ThreadGoal, continuation_ready: bool, checked: String) -> String {
    match goal.status {
        GoalStatus::Active if continuation_ready => {
            format!("Codex continues on its own · {checked}")
        }
        GoalStatus::Active => format!("Continues after your next message · {checked}"),
        _ => {
            let mut checked = checked;
            if let Some(first) = checked.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            checked
        }
    }
}

impl Composer {
    /// Goal controls need an existing Codex chat with its native thread.
    pub(super) fn goal_command_available(&self, cx: &App) -> bool {
        self.state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.config.as_ref())
            .is_some_and(|config| config.harness == HarnessId::Codex)
    }

    fn current_goal(&self, cx: &App) -> Option<ThreadGoal> {
        self.state
            .read(cx)
            .context_control
            .goal
            .as_ref()
            .and_then(|readback| readback.goal.clone())
    }

    /// `/goal [objective]` on a Codex chat becomes an explicit goal change
    /// instead of a message. Returns true when the text was consumed.
    pub(super) fn try_goal_command(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(objective) = goal_command_objective(text) else {
            return false;
        };
        if self.editing_queued.is_some() || !self.goal_command_available(cx) {
            return false;
        }
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return false;
        };
        let staged = self.staged().len()
            + self.staged_appshots().len()
            + self.input_items.get(&self.current_key).map_or(0, Vec::len)
            + self.staged_comments(cx).len();
        if staged > 0 {
            self.goal_ui.error = Some((
                chat_id,
                "A goal is text only. Remove attachments, mentions and comments, then send /goal again."
                    .into(),
            ));
            cx.notify();
            return true;
        }
        if objective.is_empty() {
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.open_goal_editor(cx);
            return true;
        }
        let operation = match self.current_goal(cx).map(|goal| goal.status) {
            None => GoalOperation::Set {
                objective: Some(objective.to_owned()),
                status: Some(GoalStatus::Active),
                token_budget: Some(GoalBudget::Unlimited),
            },
            // A finished goal restarts with the new objective.
            Some(GoalStatus::Complete) => GoalOperation::Set {
                objective: Some(objective.to_owned()),
                status: Some(GoalStatus::Active),
                token_budget: None,
            },
            // Otherwise only the objective changes; status and budget stay.
            Some(_) => GoalOperation::Set {
                objective: Some(objective.to_owned()),
                status: None,
                token_budget: None,
            },
        };
        self.submit_goal_operation(
            operation,
            PendingKind::Save,
            Some(text.trim().to_owned()),
            cx,
        );
        true
    }

    fn open_goal_editor(&mut self, cx: &mut Context<Self>) {
        if self.goal_ui.pending.is_some() {
            return;
        }
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        let goal = self.current_goal(cx);
        let objective_text = goal
            .as_ref()
            .map(|goal| goal.objective.clone())
            .unwrap_or_default();
        let budget_text = goal
            .as_ref()
            .and_then(|goal| goal.token_budget)
            .map(|tokens| tokens.to_string())
            .unwrap_or_default();
        let objective = cx.new(|cx| {
            let mut input = ComposerInput::new("What should Codex keep working toward?", cx)
                .with_text_metrics(12.5, 18.0);
            input.set_text(objective_text, cx);
            input
        });
        let budget = cx.new(|cx| {
            let mut input = ComposerInput::new("No limit", cx)
                .with_single_line()
                .with_text_metrics(12.5, 18.0);
            input.set_text(budget_text, cx);
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
        self.goal_ui.expanded.insert(chat_id.clone());
        self.goal_ui.editor = Some(GoalEditor {
            chat_id,
            objective,
            budget,
            creating: goal.is_none(),
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

    fn save_goal_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.goal_ui.editor.as_ref() else {
            return;
        };
        if self.goal_ui.pending.is_some()
            || self.state.read(cx).selected_chat.as_deref() != Some(editor.chat_id.as_str())
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
        let operation = GoalOperation::Set {
            objective: Some(objective),
            status: editor.creating.then_some(GoalStatus::Active),
            token_budget: Some(token_budget),
        };
        self.submit_goal_operation(operation, PendingKind::Save, None, cx);
    }

    fn toggle_goal_expanded(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        if !self.goal_ui.expanded.remove(&chat_id) {
            self.goal_ui.expanded.insert(chat_id);
        }
        cx.notify();
    }

    fn submit_goal_operation(
        &mut self,
        operation: GoalOperation,
        kind: PendingKind,
        command_text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.goal_ui.pending.is_some() {
            return;
        }
        let state = self.state.read(cx);
        let Some(chat_id) = state.selected_chat.clone() else {
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
            command_text,
        });
        self.goal_ui.error = None;
        self.goal_ui.task = Some(cx.spawn(async move |this, cx| {
            let intent = serde_json::json!({"kind": "goal", "operation": operation});
            let result = crate::task_commands::execute(&engine, &chat_id, intent).await;
            this.update(cx, |composer, cx| {
                let pending = composer.goal_ui.pending.take();
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
                        // Clear `/goal …` only if it is still the same text in
                        // the same chat: never erase a newer draft.
                        let issued = pending.and_then(|pending| pending.command_text);
                        if let Some(issued) = issued
                            && composer.current_key == chat_id
                            && composer.input.read(cx).text().trim() == issued
                        {
                            composer
                                .input
                                .update(cx, |input, cx| input.set_text("", cx));
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
            action,
        } = control;
        let owner = self.state.read(cx).selected_chat.clone();
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
                .child(crate::loaders::mini_mono_spinner(
                    SharedString::from(format!("{id}-spinner")),
                    2.0,
                    rest,
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
            .when(!enabled, |button| {
                button.opacity(0.45).aria_description(BUSY_REASON)
            })
            .when(icon_only, |button| {
                button
                    .tooltip(move |_, cx| cx.new(|_| AppshotActionTooltip(tooltip.clone())).into())
                    .tooltip_show_delay(Duration::from_millis(350))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.state.read(cx).selected_chat != owner {
                    return;
                }
                if this.goal_ui.pending.is_some() {
                    return;
                }
                match &action {
                    GoalAction::Edit => this.open_goal_editor(cx),
                    GoalAction::Save => this.save_goal_editor(cx),
                    GoalAction::Cancel => this.close_goal_editor(cx),
                    GoalAction::Dismiss => {
                        this.goal_ui.error = None;
                        cx.notify();
                    }
                    GoalAction::Operation(operation, kind) => {
                        this.submit_goal_operation(operation.clone(), *kind, None, cx)
                    }
                }
            }))
            .child(glyph)
            .children(text)
            .into_any_element()
    }

    /// The goal card, or `None` when this chat has nothing to show. Its
    /// placement (stacked behind the queue tray and the composer) belongs to
    /// the caller.
    pub(super) fn render_goal_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (chat_id, readback) = {
            let state = self.state.read(cx);
            let chat = state.selected_chat_row()?;
            if chat
                .config
                .as_ref()
                .is_none_or(|config| config.harness != HarnessId::Codex)
            {
                return None;
            }
            (chat.id.clone(), state.context_control.goal.clone())
        };
        let goal = readback.as_ref().and_then(|readback| readback.goal.clone());
        let editor = self
            .goal_ui
            .editor
            .as_ref()
            .filter(|editor| editor.chat_id == chat_id)
            .map(|editor| {
                (
                    editor.objective.clone(),
                    editor.budget.clone(),
                    editor.creating,
                )
            });
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
        if goal.is_none() && editor.is_none() && error.is_none() && pending.is_none() {
            return None;
        }
        if std::mem::take(&mut self.goal_ui.focus_editor)
            && let Some((objective, _, _)) = &editor
        {
            let focus = objective.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
        }
        let theme = Theme::of(cx).clone();
        let editing = editor.is_some();
        let expanded = editing || (goal.is_some() && self.goal_ui.expanded.contains(&chat_id));

        // ---- header: target, title, status, preview | clear ----
        let status = goal.as_ref().map(|goal| goal.status);
        let tone = status.map_or(theme.accent, |status| status_tone(&theme, status));
        let title = match (&goal, editing, pending) {
            (Some(_), _, _) => "Goal",
            (None, true, _) => "New goal",
            (None, false, Some(PendingKind::Save)) => "Setting goal…",
            (None, false, _) => "Goal",
        };
        let chip = status.and_then(status_chip_label).map(|label| {
            let chip_tone = status.map_or(tone, |status| status_tone(&theme, status));
            div()
                .flex_none()
                .h(px(18.0))
                .px(px(6.0))
                .flex()
                .items_center()
                .rounded(px(5.0))
                .bg(chip_tone.opacity(0.14))
                .text_size(crate::typography::ui_rems(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.readable_tone(chip_tone, 0.95))
                .child(label)
        });
        let preview = goal.as_ref().filter(|_| !expanded).map(|goal| {
            goal.objective
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
        let accent = theme.accent;
        let hover_wash = theme.ink(0.035);
        let header_main = div()
            .flex_1()
            .min_w_0()
            .h_full()
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
                    .text_size(crate::typography::ui_rems(12.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
            )
            .children(chip)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.readable_muted(0.75))
                    .children(preview),
            );
        let has_goal = goal.is_some();
        let header_lead: AnyElement = if has_goal && !editing {
            // The whole leading area is the disclosure toggle (BB).
            let owner = Some(chat_id.clone());
            let status_word = status.map_or("active", |status| match status {
                GoalStatus::Active => "active",
                GoalStatus::Paused => "paused",
                GoalStatus::Blocked => "blocked",
                GoalStatus::UsageLimited => "usage limit reached",
                GoalStatus::BudgetLimited => "budget reached",
                GoalStatus::Complete => "complete",
            });
            gpui_base::Button::new("goal-expand")
                .role(Role::Button)
                .accessibility_id("goal-expand")
                .accessibility_label(format!("Goal, {status_word}"))
                .aria_expanded(expanded)
                .aria_description(if expanded {
                    "Hide goal details"
                } else {
                    "Show goal details"
                })
                .tab_index(0)
                .flex_1()
                .min_w_0()
                .h_full()
                .pl(px(14.0))
                .pr(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded_tl(px(CARD_RADIUS - 1.0))
                .cursor_pointer()
                .hover(move |style| style.bg(hover_wash))
                .focus_visible(move |style| style.bg(accent.opacity(0.12)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.state.read(cx).selected_chat == owner {
                        this.toggle_goal_expanded(cx);
                    }
                }))
                .child(header_main)
                .child(
                    crate::icons::icon(if expanded {
                        crate::icons::ALT_ARROW_UP
                    } else {
                        crate::icons::ALT_ARROW_DOWN
                    })
                    .size(px(12.0))
                    .text_color(theme.readable_muted(0.7)),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .pl(px(14.0))
                .pr(px(10.0))
                .flex()
                .items_center()
                .child(header_main)
                .into_any_element()
        };
        let trailing: Option<AnyElement> = if has_goal {
            Some(self.goal_control(
                GoalControl {
                    id: "goal-clear",
                    label: "Clear goal".into(),
                    text: None,
                    glyph: crate::icons::CLOSE,
                    enabled: !busy,
                    spinning: pending == Some(PendingKind::Clear),
                    primary: false,
                    action: GoalAction::Operation(GoalOperation::Clear, PendingKind::Clear),
                },
                &theme,
                cx,
            ))
        } else if pending.is_some() && !editing {
            Some(
                div()
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(crate::loaders::mini_mono_spinner(
                        "goal-create-spinner",
                        2.0,
                        theme.readable_muted(0.85),
                        cx.entity_id(),
                        cx,
                    ))
                    .into_any_element(),
            )
        } else if !editing {
            Some(self.goal_control(
                GoalControl {
                    id: "goal-dismiss",
                    label: "Dismiss goal message".into(),
                    text: None,
                    glyph: crate::icons::CLOSE,
                    enabled: !busy,
                    spinning: false,
                    primary: false,
                    action: GoalAction::Dismiss,
                },
                &theme,
                cx,
            ))
        } else {
            None
        };
        let header = div()
            .id("goal-card-header")
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .child(header_lead)
            .when_some(trailing, |header, trailing| {
                // BB's divider before the dismiss control.
                header.child(
                    div()
                        .h_full()
                        .flex_none()
                        .flex()
                        .items_center()
                        .px(px(6.0))
                        .border_l_1()
                        .border_color(theme.border.opacity(0.6))
                        .child(trailing),
                )
            });

        // ---- body: objective, usage, readback, actions — or the editor ----
        let body: Option<AnyElement> = if let Some((objective, budget, creating)) = editor {
            Some(
                self.render_goal_editor(objective, budget, creating, has_goal, pending, &theme, cx),
            )
        } else if let (true, Some(goal)) = (expanded, goal.as_ref()) {
            let readback = readback.as_ref()?;
            let note = readback_note(
                goal,
                readback.continuation_ready,
                checked_ago(
                    readback.checked_at_ms,
                    chrono::Utc::now().timestamp_millis(),
                ),
            );
            let meter = goal
                .token_budget
                .filter(|budget| *budget > 0)
                .map(|budget| {
                    let fraction = (goal.tokens_used as f32 / budget as f32).clamp(0.0, 1.0);
                    let fill = if fraction >= 1.0 || goal.status == GoalStatus::BudgetLimited {
                        theme.warning
                    } else {
                        theme.accent
                    };
                    div()
                        .w(px(BUDGET_METER_WIDTH))
                        .h(px(4.0))
                        .rounded_full()
                        .bg(theme.ink(0.1))
                        .child(
                            div()
                                .h_full()
                                .w(px(BUDGET_METER_WIDTH * fraction))
                                .rounded_full()
                                .bg(fill),
                        )
                });
            let muted = theme.readable_muted(0.85);
            let meta_item = |glyph: &'static str, label: String| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        crate::icons::icon(glyph)
                            .size(px(META_GLYPH))
                            .text_color(muted),
                    )
                    .child(label)
            };
            let (toggle_label, toggle_text, toggle_glyph, toggle_status) =
                if goal.status == GoalStatus::Active {
                    (
                        "Pause goal",
                        "Pause",
                        crate::icons::PAUSE,
                        GoalStatus::Paused,
                    )
                } else {
                    (
                        "Resume goal",
                        "Resume",
                        crate::icons::PLAY,
                        GoalStatus::Active,
                    )
                };
            let edit = self.goal_control(
                GoalControl {
                    id: "goal-edit",
                    label: "Edit goal".into(),
                    text: Some("Edit"),
                    glyph: crate::icons::PEN,
                    enabled: !busy,
                    spinning: false,
                    primary: false,
                    action: GoalAction::Edit,
                },
                &theme,
                cx,
            );
            let toggle = self.goal_control(
                GoalControl {
                    id: "goal-toggle",
                    label: toggle_label.into(),
                    text: Some(toggle_text),
                    glyph: toggle_glyph,
                    enabled: !busy,
                    spinning: pending == Some(PendingKind::Toggle),
                    primary: false,
                    action: GoalAction::Operation(
                        GoalOperation::Set {
                            objective: None,
                            status: Some(toggle_status),
                            token_budget: None,
                        },
                        PendingKind::Toggle,
                    ),
                },
                &theme,
                cx,
            );
            let complete = (goal.status != GoalStatus::Complete).then(|| {
                self.goal_control(
                    GoalControl {
                        id: "goal-complete",
                        label: "Mark goal complete".into(),
                        text: Some("Mark complete"),
                        glyph: crate::icons::CHECK,
                        enabled: !busy,
                        spinning: pending == Some(PendingKind::Complete),
                        primary: false,
                        action: GoalAction::Operation(
                            GoalOperation::Set {
                                objective: None,
                                status: Some(GoalStatus::Complete),
                                token_budget: None,
                            },
                            PendingKind::Complete,
                        ),
                    },
                    &theme,
                    cx,
                )
            });
            let refresh = self.goal_control(
                GoalControl {
                    id: "goal-refresh",
                    label: "Refresh goal status".into(),
                    text: None,
                    glyph: crate::icons::REFRESH,
                    enabled: !busy,
                    spinning: pending == Some(PendingKind::Refresh),
                    primary: false,
                    action: GoalAction::Operation(GoalOperation::Get, PendingKind::Refresh),
                },
                &theme,
                cx,
            );
            Some(
                div()
                    .id("goal-card-body")
                    .border_t_1()
                    .border_color(theme.border.opacity(0.6))
                    .px(px(14.0))
                    .pt(px(10.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.5))
                            .line_height(crate::typography::ui_rems(18.0))
                            .text_color(theme.text.opacity(0.92))
                            .child(goal.objective.trim().to_owned()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(px(14.0))
                            .gap_y(px(4.0))
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(muted)
                            .child(meta_item(crate::icons::BOLT, token_usage(goal)).children(meter))
                            .child(meta_item(
                                crate::icons::CLOCK_CIRCLE,
                                elapsed(goal.time_used_seconds),
                            )),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_faint(0.9))
                            .child(note),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .ml(px(-8.0))
                            .child(edit)
                            .child(toggle)
                            .children(complete)
                            .child(div().flex_1())
                            .child(refresh),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        let error = error.map(|error| {
            div()
                .id("goal-error")
                .role(Role::Alert)
                .mx(px(12.0))
                .mb(px(10.0))
                .when(body.is_none(), |el| el.mt(px(2.0)))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.danger.opacity(0.22))
                .bg(theme.danger.opacity(0.07))
                .text_size(crate::typography::ui_rems(11.5))
                .line_height(crate::typography::ui_rems(16.0))
                .text_color(theme.readable_tone(theme.danger_muted, 0.92))
                .child(error)
        });

        let tray = div()
            .id("goal-card")
            .role(Role::Group)
            .aria_label("Goal")
            .occlude()
            .rounded_t(px(CARD_RADIUS))
            .bg(crate::popover::surface_bg(&theme))
            .border_1()
            .border_color(theme.border)
            .when(!theme.is_frost(), |el| el.shadow_lg())
            // Only the strip tucked behind the next tray needs padding.
            .pb(px(QUEUE_COMPOSER_OVERLAP))
            .flex()
            .flex_col()
            .text_color(theme.text)
            .child(header)
            .children(body)
            .children(error);
        Some(crate::frost::frosted(CARD_RADIUS, crate::frost::MENU_BLUR, tray).into_any_element())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_goal_editor(
        &self,
        objective: Entity<ComposerInput>,
        budget: Entity<ComposerInput>,
        creating: bool,
        has_goal: bool,
        pending: Option<PendingKind>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.goal_ui.pending.is_some();
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
        let cancel = self.goal_control(
            GoalControl {
                id: "goal-cancel",
                label: "Cancel goal edit".into(),
                text: Some("Cancel"),
                glyph: crate::icons::CLOSE,
                enabled: !busy,
                spinning: false,
                primary: false,
                action: GoalAction::Cancel,
            },
            theme,
            cx,
        );
        let save = self.goal_control(
            GoalControl {
                id: "goal-save",
                label: if creating { "Set goal" } else { "Save goal" }.into(),
                text: Some(if creating { "Set goal" } else { "Save" }),
                glyph: crate::icons::CHECK,
                enabled: !busy,
                spinning: pending == Some(PendingKind::Save),
                primary: true,
                action: GoalAction::Save,
            },
            theme,
            cx,
        );
        div()
            .id("goal-editor")
            .when(has_goal, |el| {
                el.border_t_1().border_color(theme.border.opacity(0.6))
            })
            .px(px(14.0))
            .pt(px(if has_goal { 10.0 } else { 2.0 }))
            .pb(px(10.0))
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
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.readable_faint(0.9))
                            .child("Enter to save · Esc to cancel"),
                    )
                    .child(cancel)
                    .child(save),
            )
            .into_any_element()
    }
}
