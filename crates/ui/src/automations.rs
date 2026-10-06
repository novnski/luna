//! Host-owned automations and PR watches (harness evolution slice 5): the
//! focused conversation's view (a right-pane surface) and the app-wide
//! manager (Settings), one panel type with two scopes. Every change is
//! previewed by the owning host and applied as that exact request; a lost
//! answer is resolved only through its receipt. Claude-native scheduled
//! prompts are a separate observation (the Goal popover) and never appear
//! here.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Task,
    Window, div, prelude::*, px,
};
use luna_proto::{
    AutomationDefinition, AutomationMutationReceipt, AutomationMutationRequest,
    AutomationOperation, AutomationPage, AutomationPreview, AutomationReadRequest,
    AutomationRecord, AutomationRun, AutomationRunState, AutomationTarget, AutomationTrigger,
};
use luna_rpc::{RpcError, methods};

use crate::button::{ButtonKind, ControlSize};
use crate::composer::ComposerInput;
use crate::icons;
use crate::popover::{self, ScrollRailHost};
use crate::settings::widgets;
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;

/// One page of definitions or runs (the host's default; its maximum is 100).
const PAGE: usize = 50;
/// The visible focused view re-reads its first page this often.
const POLL: Duration = Duration::from_secs(5);
/// A panel counts as visible while it rendered this recently.
const VISIBLE_FOR: Duration = Duration::from_secs(6);

/// Which automations a panel shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationScope {
    /// The focused conversation (definitions whose origin is this chat).
    Conversation(String),
    /// Every automation on this host (Settings).
    All,
}

/// What a panel asks its shell to do.
pub enum AutomationsEvent {
    /// Open the conversation a run delivered to (related-chat routing).
    OpenChat(String),
}

/// A run's state in words; Accepted is never completion and Unknown never
/// offers a retry.
pub fn run_state_label(state: AutomationRunState) -> (&'static str, &'static str) {
    match state {
        AutomationRunState::Claimed => (
            "Claimed",
            "Reserved for delivery; not sent yet. You can cancel it (this pauses the automation).",
        ),
        AutomationRunState::Dispatching => ("Dispatching", "Being handed to the conversation now."),
        AutomationRunState::Accepted => (
            "Accepted",
            "The provider accepted the message; the turn has not finished. Stop it from the conversation.",
        ),
        AutomationRunState::Completed => ("Completed", "The turn finished."),
        AutomationRunState::Failed => ("Failed", "Not delivered; see the reason."),
        AutomationRunState::Unknown => (
            "Unknown",
            "Delivery can't be proven either way. Inspect the conversation; it is never resent automatically.",
        ),
        AutomationRunState::Skipped => (
            "Skipped",
            "Not run (too late, or replaced by a later slot).",
        ),
        AutomationRunState::Cancelled => ("Cancelled", "Cancelled before it was sent."),
    }
}

/// A time in its automation's zone, as "2026-10-05 14:00 Europe/Bucharest"
/// (the host's own zoned formatting; the raw text when it can't be read).
pub fn when(ms: i64, zone: &str) -> String {
    let full = luna_engine::automation::format_in_zone(ms, zone);
    match (full.get(..10), full.get(10..11), full.get(11..16)) {
        (Some(date), Some("T"), Some(clock)) => format!("{date} {clock} {zone}"),
        _ => full,
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A one-shot whose instant is not after `now`. The host schedules a
/// one-shot only strictly after the current time and refuses to save a past
/// one, so a paused spent one-shot can't simply resume: it needs a new time.
fn one_shot_spent(record: &AutomationRecord, now: i64) -> bool {
    matches!(record.definition.trigger, AutomationTrigger::Once { at_ms } if at_ms <= now)
}

/// A row's enable control: Pause while enabled; Resume when paused; Edit time
/// for a paused one-shot whose time has passed (it opens this row's editor;
/// saving a new future time re-enables it). Paused future one-shots,
/// intervals, calendars and PR watches keep Resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowToggle {
    Pause,
    Resume,
    EditTime,
}

impl RowToggle {
    fn for_record(record: &AutomationRecord, now: i64) -> Self {
        if record.enabled {
            Self::Pause
        } else if one_shot_spent(record, now) {
            Self::EditTime
        } else {
            Self::Resume
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::EditTime => "Edit time",
        }
    }
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// The cadence in words, in its own zone.
pub fn describe_trigger(definition: &AutomationDefinition) -> String {
    let zone = &definition.timezone;
    match &definition.trigger {
        AutomationTrigger::Once { at_ms } => format!("Once, {}", when(*at_ms, zone)),
        AutomationTrigger::Interval { every_minutes, .. } => {
            format!("Every {every_minutes} minutes")
        }
        AutomationTrigger::Calendar {
            hour,
            minute,
            weekdays,
        } => {
            let days = if weekdays.is_empty() {
                "Every day".to_string()
            } else {
                weekdays
                    .iter()
                    .filter_map(|day| WEEKDAYS.get((*day as usize).wrapping_sub(1)))
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("{days} at {hour:02}:{minute:02} ({zone})")
        }
        AutomationTrigger::PrWatch {
            repository,
            number,
            poll_minutes,
            wake_budget,
        } => format!(
            "Watch {repository}#{number}, checked every {poll_minutes} min, up to {wake_budget} wakes"
        ),
    }
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The editor's trigger choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Once,
    Interval,
    Calendar,
    PrWatch,
}

impl TriggerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Once => "Once",
            Self::Interval => "Interval",
            Self::Calendar => "Calendar",
            Self::PrWatch => "Pull request",
        }
    }
}

/// Where a mutation flow stands.
#[derive(Debug, Clone)]
enum Phase {
    Editing,
    Previewing,
    Reviewed,
    Applying,
    /// The answer was lost: same request id, resolved only by its receipt.
    Unknown {
        checking: bool,
    },
    Error(String),
}

/// The create/edit form. The request id and automation id are fixed for
/// the dialog; a preview belongs to the exact request it was made for.
struct Editor {
    automation_id: String,
    request_id: String,
    expected_revision: u64,
    existing: bool,
    /// Editing a paused automation: saving re-enables it (the host's Save).
    resumes: bool,
    kind: TriggerKind,
    new_conversation: bool,
    weekdays: [bool; 7],
    name: Entity<ComposerInput>,
    prompt: Entity<ComposerInput>,
    timezone: Entity<ComposerInput>,
    date: Entity<ComposerInput>,
    time: Entity<ComposerInput>,
    every: Entity<ComposerInput>,
    lateness: Entity<ComposerInput>,
    repository: Entity<ComposerInput>,
    number: Entity<ComposerInput>,
    poll: Entity<ComposerInput>,
    budget: Entity<ComposerInput>,
    origin: String,
    anchor_ms: i64,
    preview: Option<(AutomationMutationRequest, AutomationPreview)>,
    phase: Phase,
}

/// Pause / Resume / Delete / Cancel occurrence: preview, then confirm.
struct ActionFlow {
    automation_id: String,
    name: String,
    request: AutomationMutationRequest,
    preview: Option<AutomationPreview>,
    phase: Phase,
}

/// One automation's run history: the first page re-read while visible,
/// later pages kept until an explicit reload.
#[derive(Default)]
struct Runs {
    first: Vec<AutomationRun>,
    more: Vec<AutomationRun>,
    first_next: Option<String>,
    more_next: Option<String>,
    loading_more: bool,
    error: Option<String>,
}

impl Runs {
    fn items(&self) -> impl Iterator<Item = &AutomationRun> {
        let known: std::collections::HashSet<&str> =
            self.first.iter().map(|run| run.id.as_str()).collect();
        self.first.iter().chain(
            self.more
                .iter()
                .filter(move |run| !known.contains(run.id.as_str())),
        )
    }
    fn next(&self) -> Option<&String> {
        if self.more.is_empty() {
            self.first_next.as_ref()
        } else {
            self.more_next.as_ref()
        }
    }
}

pub struct AutomationsPanel {
    state: Entity<AppState>,
    scope: AutomationScope,
    engine_generation: u64,
    load_seq: u64,
    first: Vec<AutomationRecord>,
    first_next: Option<String>,
    more: Vec<AutomationRecord>,
    more_next: Option<String>,
    loading: bool,
    loading_more: bool,
    error: Option<String>,
    show_deleted: bool,
    runs: HashMap<String, Runs>,
    editor: Option<Editor>,
    action: Option<ActionFlow>,
    last_render: Cell<Option<Instant>>,
    list_task: Option<Task<()>>,
    more_task: Option<Task<()>>,
    runs_tasks: HashMap<String, Task<()>>,
    flow_task: Option<Task<()>>,
    /// The panel's scroller and its floating rail (page and pane alike).
    scroll: widgets::PageScroll,
    _poll: Task<()>,
    /// QA: report the next mutation's answer as lost.
    #[cfg(feature = "appshots-fixture")]
    lose_next_mutation: bool,
}

impl EventEmitter<AutomationsEvent> for AutomationsPanel {}

fn bad(error: RpcError) -> String {
    match error {
        RpcError::Failed(message) | RpcError::BadParams(message) => message,
        other => other.to_string(),
    }
}

async fn list_page(
    engine: EngineHandle,
    request: AutomationReadRequest,
) -> Result<AutomationPage<AutomationRecord>, String> {
    let params = serde_json::to_value(&request).map_err(|error| error.to_string())?;
    engine
        .client()
        .call_as(methods::LIST_AUTOMATIONS, params)
        .await
        .map_err(bad)
}

async fn runs_page(
    engine: EngineHandle,
    request: AutomationReadRequest,
) -> Result<AutomationPage<AutomationRun>, String> {
    let params = serde_json::to_value(&request).map_err(|error| error.to_string())?;
    engine
        .client()
        .call_as(methods::LIST_AUTOMATION_RUNS, params)
        .await
        .map_err(bad)
}

async fn preview(
    engine: EngineHandle,
    request: AutomationMutationRequest,
) -> Result<AutomationPreview, String> {
    let params = serde_json::to_value(&request).map_err(|error| error.to_string())?;
    engine
        .client()
        .call_as(methods::PREVIEW_AUTOMATION, params)
        .await
        .map_err(bad)
}

/// A mutation's answer: applied, refused with the host's reason, or lost.
enum Mutation {
    Applied(Box<AutomationMutationReceipt>),
    Refused(String),
    Lost,
}

async fn mutate(engine: EngineHandle, request: AutomationMutationRequest) -> Mutation {
    let Ok(params) = serde_json::to_value(&request) else {
        return Mutation::Refused("The change could not be encoded.".into());
    };
    match engine
        .client()
        .call_as::<AutomationMutationReceipt>(methods::MUTATE_AUTOMATION, params)
        .await
    {
        Ok(receipt) => Mutation::Applied(Box::new(receipt)),
        Err(RpcError::Transport(_) | RpcError::Closed) => Mutation::Lost,
        Err(error) => Mutation::Refused(bad(error)),
    }
}

/// The receipt for a request id (`None`: still unconfirmed).
async fn receipt(
    engine: EngineHandle,
    request_id: String,
) -> Result<Option<AutomationMutationReceipt>, String> {
    engine
        .client()
        .call_as(
            methods::GET_AUTOMATION_MUTATION_RECEIPT,
            serde_json::json!({ "requestId": request_id }),
        )
        .await
        .map_err(bad)
}

impl AutomationsPanel {
    pub fn new(state: Entity<AppState>, scope: AutomationScope, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let alive = this.update(cx, |panel, cx| {
                    let visible = panel
                        .last_render
                        .get()
                        .is_some_and(|at| at.elapsed() < VISIBLE_FOR);
                    if visible && panel.list_task.is_none() {
                        panel.refresh(false, cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        let mut panel = Self {
            state,
            scope,
            engine_generation: 0,
            load_seq: 0,
            first: Vec::new(),
            first_next: None,
            more: Vec::new(),
            more_next: None,
            loading: false,
            loading_more: false,
            error: None,
            show_deleted: false,
            runs: HashMap::new(),
            editor: None,
            action: None,
            last_render: Cell::new(None),
            list_task: None,
            more_task: None,
            runs_tasks: HashMap::new(),
            flow_task: None,
            scroll: widgets::PageScroll::default(),
            _poll: poll,
            #[cfg(feature = "appshots-fixture")]
            lose_next_mutation: false,
        };
        panel.refresh(true, cx);
        panel
    }

    /// Follow the focused conversation (or stay app-wide). A new scope
    /// starts over; nothing from the previous one is kept.
    pub fn set_scope(&mut self, scope: AutomationScope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.editor = None;
        self.action = None;
        self.flow_task = None;
        self.refresh(true, cx);
    }

    fn scope_chat(&self) -> Option<&str> {
        match &self.scope {
            AutomationScope::Conversation(chat) => Some(chat),
            AutomationScope::All => None,
        }
    }

    /// The conversation's host when it is not this one (local only here).
    fn foreign_host(&self, cx: &App) -> Option<String> {
        let chat = self.scope_chat()?;
        let state = self.state.read(cx);
        let row = state.chats.iter().find(|row| row.id == chat)?;
        (state.local_device_id.as_deref() != Some(row.device_id.as_str())).then(|| {
            state
                .devices
                .iter()
                .find(|device| device.id == row.device_id)
                .map(|device| device.name.clone())
                .unwrap_or_else(|| "another device".into())
        })
    }

    fn local_host(&self, cx: &App) -> Option<String> {
        self.state.read(cx).local_device_id.clone()
    }

    fn engine(&self, cx: &App) -> Option<EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    /// Re-read the first page (`reset`: also drop later pages and runs).
    /// The latest request wins; answers for another scope or connection are
    /// dropped. A reconnect reloads everything.
    pub fn refresh(&mut self, reset: bool, cx: &mut Context<Self>) {
        let generation = self.state.read(cx).engine_generation();
        let reset = reset || generation != self.engine_generation;
        if reset {
            self.first.clear();
            self.more.clear();
            self.first_next = None;
            self.more_next = None;
            self.runs.clear();
            self.runs_tasks.clear();
            self.loading_more = false;
            self.more_task = None;
            self.error = None;
        }
        self.engine_generation = generation;
        if self.foreign_host(cx).is_some() {
            self.loading = false;
            cx.notify();
            return;
        }
        let Some(engine) = self.engine(cx) else {
            self.error = Some("Engine not connected.".into());
            cx.notify();
            return;
        };
        self.load_seq += 1;
        let seq = self.load_seq;
        let scope = self.scope.clone();
        let request = AutomationReadRequest {
            automation_id: None,
            task_id: self.scope_chat().map(str::to_string),
            after_id: None,
            limit: Some(PAGE),
            include_deleted: self.show_deleted,
        };
        self.loading = true;
        self.list_task = Some(cx.spawn(async move |this, cx| {
            let result = list_page(engine, request).await;
            this.update(cx, |panel, cx| {
                if panel.load_seq != seq {
                    return;
                }
                // The latest request settles even when its answer is dropped,
                // so polling continues.
                panel.loading = false;
                panel.list_task = None;
                if panel.scope != scope || panel.state.read(cx).engine_generation() != generation {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(page) => {
                        panel.first = page.items;
                        panel.first_next = page.next_after_id;
                        panel.error = None;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        // Expanded histories re-read their first page too.
        let expanded: Vec<String> = self.runs.keys().cloned().collect();
        for id in expanded {
            self.load_runs(&id, false, cx);
        }
        cx.notify();
    }

    fn rows(&self) -> impl Iterator<Item = &AutomationRecord> {
        let known: std::collections::HashSet<&str> =
            self.first.iter().map(|record| record.id.as_str()).collect();
        self.first.iter().chain(
            self.more
                .iter()
                .filter(move |record| !known.contains(record.id.as_str())),
        )
    }

    fn rows_next(&self) -> Option<String> {
        if self.more.is_empty() {
            self.first_next.clone()
        } else {
            self.more_next.clone()
        }
    }

    /// The next page of definitions (kept until an explicit reload).
    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(after) = self.rows_next().filter(|_| !self.loading_more) else {
            return;
        };
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let generation = self.engine_generation;
        let scope = self.scope.clone();
        let request = AutomationReadRequest {
            automation_id: None,
            task_id: self.scope_chat().map(str::to_string),
            after_id: Some(after.clone()),
            limit: Some(PAGE),
            include_deleted: self.show_deleted,
        };
        self.loading_more = true;
        self.more_task = Some(cx.spawn(async move |this, cx| {
            let result = list_page(engine, request).await;
            this.update(cx, |panel, cx| {
                if panel.scope != scope
                    || panel.engine_generation != generation
                    || panel.state.read(cx).engine_generation() != generation
                    || panel.rows_next().as_deref() != Some(after.as_str())
                {
                    return;
                }
                panel.loading_more = false;
                match result {
                    Ok(page) => {
                        panel.more.extend(page.items);
                        panel.more_next = page.next_after_id;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Toggle an automation's run history (always per automation).
    fn toggle_runs(&mut self, id: String, cx: &mut Context<Self>) {
        if self.runs.remove(&id).is_some() {
            self.runs_tasks.remove(&id);
            cx.notify();
            return;
        }
        self.runs.insert(id.clone(), Runs::default());
        self.load_runs(&id, false, cx);
    }

    /// Read an automation's run history: the first page (`more == false`)
    /// or the next page after what is loaded.
    fn load_runs(&mut self, id: &str, more: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(runs) = self.runs.get_mut(id) else {
            return;
        };
        let after = if more {
            let Some(after) = runs.next().cloned().filter(|_| !runs.loading_more) else {
                return;
            };
            runs.loading_more = true;
            Some(after)
        } else {
            None
        };
        let generation = self.engine_generation;
        let scope = self.scope.clone();
        let automation = id.to_string();
        let request = AutomationReadRequest {
            automation_id: Some(automation.clone()),
            task_id: None,
            after_id: after.clone(),
            limit: Some(PAGE),
            include_deleted: false,
        };
        let key = if more {
            format!("{id}#more")
        } else {
            id.to_string()
        };
        self.runs_tasks.insert(
            key,
            cx.spawn(async move |this, cx| {
                let result = runs_page(engine, request).await;
                this.update(cx, |panel, cx| {
                    if panel.scope != scope
                        || panel.engine_generation != generation
                        || panel.state.read(cx).engine_generation() != generation
                    {
                        return;
                    }
                    let Some(runs) = panel.runs.get_mut(&automation) else {
                        return;
                    };
                    match (result, after) {
                        (Ok(page), None) => {
                            runs.first = page.items;
                            runs.first_next = page.next_after_id;
                            runs.error = None;
                        }
                        (Ok(page), Some(after)) => {
                            runs.loading_more = false;
                            if runs.next().map(String::as_str) == Some(after.as_str()) {
                                runs.more.extend(page.items);
                                runs.more_next = page.next_after_id;
                            }
                        }
                        (Err(error), _) => {
                            runs.loading_more = false;
                            runs.error = Some(error);
                        }
                    }
                    cx.notify();
                })
                .ok();
            }),
        );
        cx.notify();
    }

    // ---- editor ----

    fn input(placeholder: &str, text: &str, cx: &mut Context<Self>) -> Entity<ComposerInput> {
        let placeholder = placeholder.to_string();
        let text = text.to_string();
        cx.new(move |cx| {
            let mut input = ComposerInput::new(placeholder, cx);
            if !text.is_empty() {
                input.set_text(text, cx);
            }
            input
        })
    }

    /// Open the editor: a new automation for the focused conversation, or an
    /// existing one (its own fields, its revision).
    fn open_editor(&mut self, existing: Option<AutomationRecord>, cx: &mut Context<Self>) {
        let origin = existing
            .as_ref()
            .map(|record| record.definition.target.origin_task_id().to_string())
            .or_else(|| self.scope_chat().map(str::to_string));
        if let Some(host) = self.foreign_host(cx) {
            self.error = Some(format!(
                "This conversation's automations are managed on {host}; LUNA must be open there."
            ));
            cx.notify();
            return;
        }
        let Some(origin) = origin else {
            self.error = Some("Open a conversation to create an automation from it.".into());
            cx.notify();
            return;
        };
        let zone = existing
            .as_ref()
            .map(|record| record.definition.timezone.clone())
            .or_else(luna_engine::automation::system_timezone)
            .unwrap_or_else(|| "UTC".into());
        let definition = existing.as_ref().map(|record| &record.definition);
        let (kind, date, time, every, weekdays, repo, number, poll, budget) =
            match definition.map(|definition| &definition.trigger) {
                Some(AutomationTrigger::Once { at_ms }) => {
                    let shown = luna_engine::automation::format_in_zone(*at_ms, &zone);
                    let date = shown.get(..10).unwrap_or_default().to_string();
                    let time = shown.get(11..16).unwrap_or_default().to_string();
                    (
                        TriggerKind::Once,
                        date,
                        time,
                        String::new(),
                        [false; 7],
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                }
                Some(AutomationTrigger::Interval { every_minutes, .. }) => (
                    TriggerKind::Interval,
                    String::new(),
                    String::new(),
                    every_minutes.to_string(),
                    [false; 7],
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ),
                Some(AutomationTrigger::Calendar {
                    hour,
                    minute,
                    weekdays,
                }) => {
                    let mut days = [false; 7];
                    for day in weekdays {
                        if (1..=7).contains(day) {
                            days[(*day - 1) as usize] = true;
                        }
                    }
                    (
                        TriggerKind::Calendar,
                        String::new(),
                        format!("{hour:02}:{minute:02}"),
                        String::new(),
                        days,
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                }
                Some(AutomationTrigger::PrWatch {
                    repository,
                    number,
                    poll_minutes,
                    wake_budget,
                }) => (
                    TriggerKind::PrWatch,
                    String::new(),
                    String::new(),
                    String::new(),
                    [false; 7],
                    repository.clone(),
                    number.to_string(),
                    poll_minutes.to_string(),
                    wake_budget.to_string(),
                ),
                None => (
                    TriggerKind::Once,
                    String::new(),
                    String::new(),
                    "60".into(),
                    [false; 7],
                    String::new(),
                    String::new(),
                    "15".into(),
                    "10".into(),
                ),
            };
        let editor = Editor {
            automation_id: existing
                .as_ref()
                .map(|record| record.id.clone())
                .unwrap_or_else(|| format!("auto-{}", uuid::Uuid::new_v4())),
            request_id: format!("req-{}", uuid::Uuid::new_v4()),
            expected_revision: existing.as_ref().map_or(0, |record| record.revision),
            existing: existing.is_some(),
            resumes: existing.as_ref().is_some_and(|record| !record.enabled),
            kind,
            new_conversation: matches!(
                definition.map(|definition| &definition.target),
                Some(AutomationTarget::NewConversation { .. })
            ),
            weekdays,
            name: Self::input("Name", definition.map_or("", |d| d.name.as_str()), cx),
            prompt: Self::input(
                "What to send",
                definition.map_or("", |d| d.prompt.as_str()),
                cx,
            ),
            timezone: Self::input("IANA time zone, e.g. Europe/Bucharest", &zone, cx),
            date: Self::input("YYYY-MM-DD", &date, cx),
            time: Self::input("HH:MM", &time, cx),
            every: Self::input("Minutes", &every, cx),
            lateness: Self::input(
                "Minutes",
                &definition
                    .map_or(10, |d| d.max_lateness_ms / 60_000)
                    .to_string(),
                cx,
            ),
            repository: Self::input("owner/repository", &repo, cx),
            number: Self::input("PR number", &number, cx),
            poll: Self::input("Minutes", &poll, cx),
            budget: Self::input("Wakes", &budget, cx),
            origin,
            anchor_ms: match definition.map(|definition| &definition.trigger) {
                Some(AutomationTrigger::Interval { anchor_ms, .. }) => *anchor_ms,
                _ => now_ms(),
            },
            preview: None,
            phase: Phase::Editing,
        };
        self.editor = Some(editor);
        self.action = None;
        cx.notify();
    }

    /// The editor's fields as a definition (local limits first; the host
    /// validates again).
    fn build_definition(editor: &Editor, cx: &App) -> Result<AutomationDefinition, String> {
        let text = |input: &Entity<ComposerInput>| input.read(cx).text().trim().to_string();
        let number = |input: &Entity<ComposerInput>, what: &str| {
            text(input)
                .parse::<u64>()
                .map_err(|_| format!("Enter a whole number for {what}."))
        };
        let name = text(&editor.name);
        let prompt = editor.prompt.read(cx).text().to_string();
        if name.is_empty() || name.len() > 160 {
            return Err("Enter a name (up to 160 bytes).".into());
        }
        if prompt.trim().is_empty() || prompt.len() > 32 * 1024 {
            return Err("Enter what to send (up to 32 KiB).".into());
        }
        let timezone = text(&editor.timezone);
        let lateness = number(&editor.lateness, "the missed-run tolerance")?;
        if lateness > 24 * 60 {
            return Err("The missed-run tolerance is at most 24 hours.".into());
        }
        let clock = |value: String| -> Result<(i8, i8), String> {
            let (hour, minute) = value
                .split_once(':')
                .ok_or_else(|| "Enter the time as HH:MM.".to_string())?;
            let hour: i8 = hour
                .parse()
                .map_err(|_| "Enter the time as HH:MM.".to_string())?;
            let minute: i8 = minute
                .parse()
                .map_err(|_| "Enter the time as HH:MM.".to_string())?;
            if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
                return Err("Enter a time between 00:00 and 23:59.".into());
            }
            Ok((hour, minute))
        };
        let trigger = match editor.kind {
            TriggerKind::Once => {
                let date = text(&editor.date);
                let mut parts = date.split('-');
                let (Some(year), Some(month), Some(day), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err("Enter the date as YYYY-MM-DD.".into());
                };
                let parse = |value: &str| {
                    value
                        .parse::<i16>()
                        .map_err(|_| "Enter the date as YYYY-MM-DD.".to_string())
                };
                let (hour, minute) = clock(text(&editor.time))?;
                let at_ms = luna_engine::automation::zoned_ms(
                    &timezone,
                    parse(year)?,
                    parse(month)? as i8,
                    parse(day)? as i8,
                    hour,
                    minute,
                )?;
                AutomationTrigger::Once { at_ms }
            }
            TriggerKind::Interval => {
                let every = number(&editor.every, "the interval")?;
                if every == 0 || every > u32::MAX as u64 {
                    return Err("Enter an interval of at least one minute.".into());
                }
                AutomationTrigger::Interval {
                    anchor_ms: editor.anchor_ms,
                    every_minutes: every as u32,
                }
            }
            TriggerKind::Calendar => {
                let (hour, minute) = clock(text(&editor.time))?;
                AutomationTrigger::Calendar {
                    hour,
                    minute,
                    weekdays: editor
                        .weekdays
                        .iter()
                        .enumerate()
                        .filter(|(_, on)| **on)
                        .map(|(index, _)| index as i8 + 1)
                        .collect(),
                }
            }
            TriggerKind::PrWatch => {
                let repository = text(&editor.repository);
                let valid_repo = repository.split_once('/').is_some_and(|(owner, repo)| {
                    !owner.is_empty() && !repo.is_empty() && !repo.contains('/')
                });
                if !valid_repo {
                    return Err("Enter the repository as owner/repository.".into());
                }
                let pr = number(&editor.number, "the pull request number")?;
                let poll = number(&editor.poll, "the check interval")?;
                let budget = number(&editor.budget, "the wake budget")?;
                if !(1..=1440).contains(&poll) {
                    return Err("Check every 1 to 1440 minutes.".into());
                }
                if !(1..=100).contains(&budget) {
                    return Err("Allow 1 to 100 wakes.".into());
                }
                AutomationTrigger::PrWatch {
                    repository,
                    number: pr,
                    poll_minutes: poll as u32,
                    wake_budget: budget as u32,
                }
            }
        };
        Ok(AutomationDefinition {
            name,
            prompt,
            timezone,
            target: if editor.new_conversation {
                AutomationTarget::NewConversation {
                    origin_task_id: editor.origin.clone(),
                }
            } else {
                AutomationTarget::Existing {
                    task_id: editor.origin.clone(),
                }
            },
            trigger,
            max_lateness_ms: (lateness * 60_000) as i64,
        })
    }

    /// The exact request the editor would send now (`preview_id` empty).
    fn editor_request(&self, cx: &App) -> Result<AutomationMutationRequest, String> {
        let editor = self.editor.as_ref().ok_or("No editor")?;
        let owner = self.local_host(cx).ok_or("No local host")?;
        Ok(AutomationMutationRequest {
            request_id: editor.request_id.clone(),
            automation_id: editor.automation_id.clone(),
            owner_host_id: owner,
            expected_revision: editor.expected_revision,
            operation: AutomationOperation::Save {
                definition: Self::build_definition(editor, cx)?,
            },
            preview_id: String::new(),
        })
    }

    /// The reviewed preview still describes exactly what Apply would send.
    fn editor_preview_current(&self, cx: &App) -> bool {
        let Some((reviewed, _)) = self
            .editor
            .as_ref()
            .and_then(|editor| editor.preview.as_ref())
        else {
            return false;
        };
        self.editor_request(cx)
            .is_ok_and(|request| &request == reviewed)
    }

    /// The reviewed preview's token is past its expiry (Review again).
    fn editor_preview_expired(&self) -> bool {
        self.editor
            .as_ref()
            .and_then(|editor| editor.preview.as_ref())
            .is_some_and(|(_, preview)| preview.expires_at_ms <= now_ms())
    }

    fn review_editor(&mut self, cx: &mut Context<Self>) {
        let request = match self.editor_request(cx) {
            Ok(request) => request,
            Err(error) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.phase = Phase::Error(error);
                }
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        if matches!(editor.phase, Phase::Applying | Phase::Unknown { .. }) {
            return;
        }
        editor.phase = Phase::Previewing;
        editor.preview = None;
        let generation = self.engine_generation;
        let request_id = editor.request_id.clone();
        self.flow_task = Some(cx.spawn(async move |this, cx| {
            let result = preview(engine, request.clone()).await;
            this.update(cx, |panel, cx| {
                if panel.state.read(cx).engine_generation() != generation {
                    return;
                }
                let Some(editor) = panel
                    .editor
                    .as_mut()
                    .filter(|editor| editor.request_id == request_id)
                else {
                    return;
                };
                match result {
                    Ok(preview) => {
                        editor.preview = Some((request, preview));
                        editor.phase = Phase::Reviewed;
                    }
                    Err(error) => editor.phase = Phase::Error(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Apply exactly the reviewed request (or, explicitly, send that same
    /// request again after a lost answer; never a new id).
    fn apply_editor(&mut self, cx: &mut Context<Self>) {
        let resend = matches!(
            self.editor.as_ref().map(|editor| &editor.phase),
            Some(Phase::Unknown { checking: false })
        );
        if !resend && (!self.editor_preview_current(cx) || self.editor_preview_expired()) {
            return;
        }
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let Some((reviewed, preview)) = editor.preview.clone() else {
            return;
        };
        let mut request = reviewed;
        request.preview_id = preview.preview_id;
        editor.phase = Phase::Applying;
        let generation = self.engine_generation;
        let request_id = request.request_id.clone();
        #[cfg(feature = "appshots-fixture")]
        let lose = std::mem::take(&mut self.lose_next_mutation);
        self.flow_task = Some(cx.spawn(async move |this, cx| {
            let answer = mutate(engine, request).await;
            #[cfg(feature = "appshots-fixture")]
            let answer = match answer {
                Mutation::Applied(_) if lose => Mutation::Lost,
                answer => answer,
            };
            this.update(cx, |panel, cx| {
                panel.settle_editor(&request_id, generation, answer, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn settle_editor(
        &mut self,
        request_id: &str,
        generation: u64,
        answer: Mutation,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).engine_generation() != generation {
            return;
        }
        let Some(editor) = self
            .editor
            .as_mut()
            .filter(|editor| editor.request_id == request_id)
        else {
            return;
        };
        match answer {
            Mutation::Applied(receipt)
                if receipt.request_id == request_id
                    && receipt.record.id == editor.automation_id =>
            {
                self.editor = None;
                self.refresh(false, cx);
            }
            Mutation::Applied(_) => editor.phase = Phase::Unknown { checking: false },
            Mutation::Refused(reason) => editor.phase = Phase::Error(reason),
            Mutation::Lost => editor.phase = Phase::Unknown { checking: false },
        }
        cx.notify();
    }

    /// Check status: the receipt for the same request id (read only).
    fn check_editor(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let Phase::Unknown { checking } = &mut editor.phase else {
            return;
        };
        if *checking {
            return;
        }
        *checking = true;
        let request_id = editor.request_id.clone();
        let generation = self.engine_generation;
        self.flow_task = Some(cx.spawn(async move |this, cx| {
            let result = receipt(engine, request_id.clone()).await;
            this.update(cx, |panel, cx| {
                let answer = match result {
                    Ok(Some(receipt)) => Mutation::Applied(Box::new(receipt)),
                    _ => Mutation::Lost,
                };
                panel.settle_editor(&request_id, generation, answer, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- row actions ----

    fn begin_action(
        &mut self,
        record: &AutomationRecord,
        operation: AutomationOperation,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.local_host(cx) else {
            return;
        };
        self.editor = None;
        self.action = Some(ActionFlow {
            automation_id: record.id.clone(),
            name: record.definition.name.clone(),
            request: AutomationMutationRequest {
                request_id: format!("req-{}", uuid::Uuid::new_v4()),
                automation_id: record.id.clone(),
                owner_host_id: owner,
                expected_revision: record.revision,
                operation,
                preview_id: String::new(),
            },
            preview: None,
            phase: Phase::Previewing,
        });
        self.review_action(cx);
    }

    fn review_action(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(action) = self.action.as_mut() else {
            return;
        };
        action.phase = Phase::Previewing;
        let request = action.request.clone();
        let generation = self.engine_generation;
        self.flow_task = Some(cx.spawn(async move |this, cx| {
            let result = preview(engine, request.clone()).await;
            this.update(cx, |panel, cx| {
                if panel.state.read(cx).engine_generation() != generation {
                    return;
                }
                let Some(action) = panel
                    .action
                    .as_mut()
                    .filter(|action| action.request == request)
                else {
                    return;
                };
                match result {
                    Ok(preview) => {
                        action.preview = Some(preview);
                        action.phase = Phase::Reviewed;
                    }
                    Err(error) => action.phase = Phase::Error(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn confirm_action(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(action) = self.action.as_mut() else {
            return;
        };
        let resend = matches!(action.phase, Phase::Unknown { checking: false });
        let expired = action
            .preview
            .as_ref()
            .is_none_or(|preview| preview.expires_at_ms <= now_ms());
        if !resend && (!matches!(action.phase, Phase::Reviewed) || expired) {
            return;
        }
        let Some(preview) = action.preview.clone() else {
            return;
        };
        let mut request = action.request.clone();
        request.preview_id = preview.preview_id;
        action.phase = Phase::Applying;
        let generation = self.engine_generation;
        let request_id = request.request_id.clone();
        #[cfg(feature = "appshots-fixture")]
        let lose = std::mem::take(&mut self.lose_next_mutation);
        self.flow_task = Some(cx.spawn(async move |this, cx| {
            let answer = mutate(engine, request).await;
            #[cfg(feature = "appshots-fixture")]
            let answer = match answer {
                Mutation::Applied(_) if lose => Mutation::Lost,
                answer => answer,
            };
            this.update(cx, |panel, cx| {
                panel.settle_action(&request_id, generation, answer, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn settle_action(
        &mut self,
        request_id: &str,
        generation: u64,
        answer: Mutation,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).engine_generation() != generation {
            return;
        }
        let Some(action) = self
            .action
            .as_mut()
            .filter(|action| action.request.request_id == request_id)
        else {
            return;
        };
        match answer {
            Mutation::Applied(receipt)
                if receipt.request_id == request_id
                    && receipt.record.id == action.request.automation_id =>
            {
                self.action = None;
                self.refresh(false, cx);
            }
            Mutation::Applied(_) => action.phase = Phase::Unknown { checking: false },
            Mutation::Refused(reason) => action.phase = Phase::Error(reason),
            Mutation::Lost => action.phase = Phase::Unknown { checking: false },
        }
        cx.notify();
    }

    fn check_action(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let Some(action) = self.action.as_mut() else {
            return;
        };
        let Phase::Unknown { checking } = &mut action.phase else {
            return;
        };
        if *checking {
            return;
        }
        *checking = true;
        let request_id = action.request.request_id.clone();
        let generation = self.engine_generation;
        self.flow_task = Some(cx.spawn(async move |this, cx| {
            let result = receipt(engine, request_id.clone()).await;
            this.update(cx, |panel, cx| {
                let answer = match result {
                    Ok(Some(receipt)) => Mutation::Applied(Box::new(receipt)),
                    _ => Mutation::Lost,
                };
                panel.settle_action(&request_id, generation, answer, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// The row's enable control, decided again at activation: a paused
    /// one-shot whose time passed since the last render edits its time
    /// instead of attempting a Resume that could schedule nothing.
    fn activate_toggle(&mut self, record: &AutomationRecord, cx: &mut Context<Self>) -> RowToggle {
        let toggle = RowToggle::for_record(record, now_ms());
        match toggle {
            RowToggle::Pause => self.begin_action(record, AutomationOperation::Pause, cx),
            RowToggle::Resume => self.begin_action(record, AutomationOperation::Resume, cx),
            RowToggle::EditTime => self.open_editor(Some(record.clone()), cx),
        }
        toggle
    }

    fn chat_title(&self, id: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .and_then(|chat| chat.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled chat".into())
    }

    fn host_name(&self, id: &str, cx: &App) -> String {
        let state = self.state.read(cx);
        state
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.name.clone())
            .unwrap_or_else(|| {
                if state.local_device_id.as_deref() == Some(id) {
                    "this device".into()
                } else {
                    "another device".into()
                }
            })
    }

    /// A definition's status line.
    fn status_line(record: &AutomationRecord) -> String {
        let zone = &record.definition.timezone;
        if record.deleted {
            return "Deleted".into();
        }
        if !record.enabled {
            if one_shot_spent(record, now_ms()) {
                return "Paused. This one-shot's time has passed: choose a new time to resume."
                    .into();
            }
            return "Paused. No new runs start; accepted work continues under its task controls."
                .into();
        }
        match record.next_due_ms {
            Some(next) => format!("Active · next {}", when(next, zone)),
            None => "Active · nothing scheduled".into(),
        }
    }
}

impl ScrollRailHost for AutomationsPanel {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for AutomationsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.last_render.set(Some(Instant::now()));
        // A reconnect reloads saved definitions and receipts.
        if self.state.read(cx).engine_generation() != self.engine_generation {
            self.refresh(true, cx);
        }
        let theme = Theme::of(cx).clone();
        // Settings → Automations is a settings page; the right pane keeps
        // its surface toolbar. Both share the list, rows and editor below.
        if matches!(self.scope, AutomationScope::All) {
            self.render_page(&theme, cx)
        } else {
            self.render_pane(&theme, cx)
        }
    }
}

/// Text input frame for the editor: the shared dialog field on interface
/// rems and theme fills, so it sits in a section card beside kit controls.
fn input_frame(input: &Entity<ComposerInput>, theme: &Theme) -> gpui::Div {
    crate::popover::dialog_field(input.clone().into_any_element())
        .px(crate::typography::ui_rems(10.0))
        .py(crate::typography::ui_rems(6.0))
        .rounded(px(8.0))
        .border_color(theme.separator(true))
        .bg(theme.fill(crate::theme::FillTier::Quaternary))
        .text_size(crate::typography::ui_rems(13.0))
}

/// An inline notice inside a card, on the rows' text edge.
fn card_notice(strip: gpui::Div) -> gpui::Div {
    div()
        .w_full()
        .px(crate::typography::ui_rems(widgets::ROW_PAD_X))
        .pb(crate::typography::ui_rems(widgets::ROW_PAD_Y))
        .child(strip.mt(px(0.0)))
}

impl AutomationsPanel {
    /// Settings → Automations: the page frame (title bar with the page's
    /// actions, subtitle), the editor when one is open, then the list card,
    /// in the page's own scroller with the floating rail.
    fn render_page(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let actions = self.header_actions(ControlSize::Md, theme, cx);
        let editor = self.editor.is_some().then(|| self.render_editor(theme, cx));
        let list = self.render_list("On this device", theme, cx);
        let scrollbar = popover::rail(self, "automations-page-scrollbar", theme, cx);
        div()
            .id("automations-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.scroll.set_list_hovered(*hovered) {
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id("automations-list")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(theme, "Automations", None, actions))
                            .child(widgets::page_subtitle(
                                theme,
                                "Runs on this device while LUNA is open. Claude's own scheduled \
                                 prompts are listed in the Goal popover, not here.",
                            ))
                            .children(editor)
                            .child(list),
                    ),
            )
            .children(scrollbar)
            .into_any_element()
    }

    /// The right pane's surface for the focused conversation: the surface
    /// toolbar (title, then the same actions at `Sm`) over a scrolling body
    /// holding the editor and the list card.
    fn render_pane(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let title = match &self.scope {
            AutomationScope::Conversation(chat) => {
                format!("Automations · {}", self.chat_title(chat, cx))
            }
            AutomationScope::All => "Automations on this device".into(),
        };
        let foreign = self.foreign_host(cx);
        let actions = if foreign.is_some() {
            Vec::new()
        } else {
            self.header_actions(ControlSize::Sm, theme, cx)
        };
        let toolbar = crate::surface_chrome::toolbar(theme)
            .child(
                // The chat title is user data: it ellipsizes before the
                // actions are pushed out of the pane.
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(title)),
            )
            .children(actions);
        let mut body = div()
            .id("automations-list")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll.scroll)
            .px(crate::typography::ui_rems(12.0))
            .pt(crate::typography::ui_rems(4.0))
            .pb(crate::typography::ui_rems(16.0))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(12.0))
            .child(widgets::row_description(
                theme,
                "Runs on this device while LUNA is open. Claude's own scheduled prompts are \
                 listed in the Goal popover, not here.",
            ));
        if let Some(host) = foreign {
            body = body.child(
                widgets::section_card(theme).mt(px(0.0)).child(
                    widgets::card_row(theme, true)
                        .child(widgets::row_tile(theme, icons::MONITOR))
                        .child(widgets::row_text(
                            theme,
                            format!("Managed on {host}"),
                            Some(
                                format!(
                                    "This conversation's automations are managed on {host}; LUNA \
                                     must be open there. They can't be viewed or changed from \
                                     this device."
                                )
                                .into(),
                            ),
                        )),
                ),
            );
        } else {
            if self.editor.is_some() {
                body = body.child(self.render_editor(theme, cx).mt(px(0.0)));
            }
            body = body.child(self.render_list("This conversation", theme, cx).mt(px(0.0)));
        }
        let scrollbar = popover::rail(self, "automations-pane-scrollbar", theme, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(crate::typography::ui_rems(12.0))
            .child(toolbar)
            .child(
                div()
                    .id("automations-pane-host")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if this.scroll.set_list_hovered(*hovered) {
                            cx.notify();
                        }
                    }))
                    .child(body)
                    .children(scrollbar),
            )
            .into_any_element()
    }

    /// The panel's own actions, in the page title bar (`Md`) or the pane
    /// toolbar (`Sm`): New automation (only where a conversation can own
    /// one), the Show deleted filter and Reload.
    fn header_actions(
        &self,
        size: ControlSize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut actions = Vec::new();
        if self.scope_chat().is_some() && self.editor.is_none() {
            actions.push(
                crate::button::button("automation-new", ButtonKind::Secondary, size, true, theme)
                    .aria_label("New automation")
                    .child(crate::button::glyph(icons::PLUS, size))
                    .child("New")
                    .on_click(cx.listener(|this, _, _, cx| this.open_editor(None, cx)))
                    .into_any_element(),
            );
        }
        actions.push(
            crate::button::chip("automation-deleted", self.show_deleted, size, true, theme)
                .aria_label("Show deleted automations")
                .child("Show deleted")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_deleted = !this.show_deleted;
                    this.refresh(true, cx);
                }))
                .into_any_element(),
        );
        actions.push(
            crate::button::icon_button("automation-reload", icons::REFRESH, size, true, theme)
                .aria_label("Reload automations")
                .tooltip(popover::text_tooltip("Reload automations"))
                .tooltip_show_delay(popover::TOOLTIP_DELAY)
                .on_click(cx.listener(|this, _, _, cx| this.refresh(true, cx)))
                .into_any_element(),
        );
        actions
    }

    /// The list card: a header naming the scope and the count, the error
    /// strip, then loading skeletons, the empty row or one row per
    /// automation, and Load more as the last row.
    fn render_list(&self, title: &'static str, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let rows: Vec<AutomationRecord> = self.rows().cloned().collect();
        let more = self.rows_next().is_some();
        let summary = match rows.len() {
            0 => None,
            1 => Some("1 automation".to_string()),
            n => Some(format!("{n} automations")),
        }
        .map(|count| {
            if more {
                format!("{count} · more available")
            } else {
                count
            }
        });
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            title,
            summary.map(SharedString::from),
            Vec::new(),
        ));
        if let Some(error) = &self.error {
            card = card.child(card_notice(widgets::error_strip(theme, error.clone())));
        }
        if self.loading && self.first.is_empty() {
            card = card.child(
                div()
                    .w_full()
                    .border_t_1()
                    .border_color(theme.border)
                    .px(crate::typography::ui_rems(widgets::ROW_PAD_X))
                    .py(crate::typography::ui_rems(widgets::ROW_PAD_Y))
                    .child(popover::skeleton_rows(
                        "automations-loading",
                        theme,
                        3,
                        cx.entity_id(),
                        cx,
                    )),
            );
        } else if rows.is_empty() {
            let creatable = self.scope_chat().is_some() && self.editor.is_none();
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_tile(theme, icons::CLOCK_CIRCLE))
                    .child(widgets::row_text(
                        theme,
                        "No automations yet",
                        Some(
                            if creatable {
                                "Send this conversation a message on a schedule, or wake it when \
                                 a pull request changes."
                            } else {
                                "Create one from a conversation's Automations pane."
                            }
                            .into(),
                        ),
                    ))
                    .when(creatable, |row| {
                        row.child(
                            widgets::row_controls().child(
                                crate::button::button(
                                    "automation-new-empty",
                                    ButtonKind::Secondary,
                                    ControlSize::Sm,
                                    true,
                                    theme,
                                )
                                .aria_label("New automation")
                                .child(crate::button::glyph(icons::PLUS, ControlSize::Sm))
                                .child("New automation")
                                .on_click(cx.listener(|this, _, _, cx| this.open_editor(None, cx))),
                            ),
                        )
                    }),
            );
        }
        for record in rows {
            card = card.child(self.render_record(&record, theme, cx));
        }
        if more {
            let enabled = !self.loading_more;
            card = card.child(
                widgets::card_row(theme, false).child(
                    widgets::row_controls().child(
                        crate::button::button(
                            "automation-more",
                            ButtonKind::Ghost,
                            ControlSize::Sm,
                            enabled,
                            theme,
                        )
                        .aria_label("Load more automations")
                        .child(if enabled { "Load more" } else { "Loading…" })
                        .when(enabled, |button| {
                            button.on_click(cx.listener(|this, _, _, cx| this.load_more(cx)))
                        }),
                    ),
                ),
            );
        }
        card
    }

    /// One automation as a list row: name and status badge, the schedule,
    /// target and host lines, and the trailing kit actions; its confirm
    /// flow and run history open as full-width blocks under it.
    fn render_record(
        &self,
        record: &AutomationRecord,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let definition = &record.definition;
        let target = match &definition.target {
            AutomationTarget::Existing { task_id } => {
                format!("Sends to {}", self.chat_title(task_id, cx))
            }
            AutomationTarget::NewConversation { origin_task_id } => format!(
                "Starts a new conversation each run (from {})",
                self.chat_title(origin_task_id, cx)
            ),
        };
        let status_badge = if record.deleted {
            widgets::badge(theme, "Deleted")
        } else if !record.enabled {
            widgets::badge(theme, "Paused")
        } else {
            widgets::badge_active(theme, "Active")
        };
        let mut text = div()
            .flex_1()
            .min_w(crate::typography::ui_rems(widgets::ROW_TEXT_MIN))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(2.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_x(crate::typography::ui_rems(8.0))
                    .gap_y(crate::typography::ui_rems(4.0))
                    .child(widgets::row_title(theme, definition.name.clone()))
                    .child(status_badge),
            )
            .child(
                widgets::row_description(theme, Self::status_line(record)).text_color(theme.text),
            )
            .child(widgets::row_description(
                theme,
                format!(
                    "{} · time zone {}",
                    describe_trigger(definition),
                    definition.timezone
                ),
            ))
            .child(widgets::row_description(theme, target))
            .child(widgets::row_description(
                theme,
                format!(
                    "Runs on {} while LUNA is open · access up to {} · revision {}",
                    self.host_name(&record.owner_host_id, cx),
                    crate::execution_view::permission_title(record.authority.permission_ceiling),
                    record.revision
                ),
            ));
        if let Some(watch) = &record.watch
            && let AutomationTrigger::PrWatch { wake_budget, .. } = &definition.trigger
        {
            text = text.child(widgets::row_description(
                theme,
                format!(
                    "{} of {} wakes used · {} · LUNA never posts GitHub comments",
                    watch.wakes_used,
                    wake_budget,
                    plural(u64::from(watch.failures), "read failure", "read failures"),
                ),
            ));
            if let Some(error) = &watch.last_error {
                text = text.child(
                    widgets::row_description(theme, format!("Last error: {error}"))
                        .text_color(theme.readable_tone(theme.warning, 1.0)),
                );
            }
        }
        let mut row = widgets::card_row(theme, false)
            .id(SharedString::from(format!("automation-{}", record.id)))
            .child(text);
        // Row actions: History (a disclosure), Edit, the enable toggle, and
        // Delete (Secondary: it opens a confirm, never deletes on click).
        if !record.deleted {
            let history_open = self.runs.contains_key(&record.id);
            let id = record.id.clone();
            let edit = record.clone();
            let toggled = record.clone();
            let deleted = record.clone();
            let toggle = RowToggle::for_record(record, now_ms());
            row = row.child(
                widgets::row_controls()
                    .child(
                        crate::button::button(
                            format!("automation-history-{}", record.id),
                            ButtonKind::Ghost,
                            ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label(if history_open {
                            "Hide history"
                        } else {
                            "History"
                        })
                        .aria_expanded(history_open)
                        .child(if history_open {
                            "Hide history"
                        } else {
                            "History"
                        })
                        .child(crate::button::glyph(
                            if history_open {
                                icons::ALT_ARROW_UP
                            } else {
                                icons::ALT_ARROW_DOWN
                            },
                            ControlSize::Sm,
                        ))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_runs(id.clone(), cx)),
                        ),
                    )
                    .child(
                        crate::button::button(
                            format!("automation-edit-{}", record.id),
                            ButtonKind::Secondary,
                            ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label("Edit")
                        .child("Edit")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_editor(Some(edit.clone()), cx)
                        })),
                    )
                    .child(
                        crate::button::button(
                            format!("automation-toggle-{}", record.id),
                            ButtonKind::Secondary,
                            ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label(toggle.label())
                        .child(crate::button::glyph(
                            match toggle {
                                RowToggle::Pause => icons::PAUSE,
                                RowToggle::Resume => icons::PLAY,
                                RowToggle::EditTime => icons::CALENDAR,
                            },
                            ControlSize::Sm,
                        ))
                        .child(toggle.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_toggle(&toggled, cx);
                        })),
                    )
                    .child(
                        crate::button::button(
                            format!("automation-delete-{}", record.id),
                            ButtonKind::Secondary,
                            ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label("Delete")
                        .child("Delete")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.begin_action(&deleted, AutomationOperation::Delete, cx)
                        })),
                    ),
            );
        }
        if let Some(action) = self
            .action
            .as_ref()
            .filter(|action| action.automation_id == record.id)
        {
            row = row.child(widgets::row_block().child(self.render_action(action, theme, cx)));
        }
        if let Some(runs) = self.runs.get(&record.id) {
            row = row.child(widgets::row_block().child(self.render_runs(record, runs, theme, cx)));
        }
        row.into_any_element()
    }

    /// The confirm flow for Pause / Resume / Delete / Cancel occurrence: a
    /// recessed well under its row with what will happen, the host's review,
    /// and the actions right-aligned (Confirm last; Delete confirms in the
    /// danger kind).
    fn render_action(
        &self,
        action: &ActionFlow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let warning = theme.readable_tone(theme.warning, 1.0);
        let danger = theme.readable_tone(theme.danger_muted, 1.0);
        let what = match &action.request.operation {
            AutomationOperation::Pause => format!(
                "Pause \"{}\". No new runs start; work a provider already accepted continues under its normal task controls.",
                action.name
            ),
            AutomationOperation::Resume => format!("Resume \"{}\".", action.name),
            AutomationOperation::Delete => format!(
                "Delete \"{}\". It stops scheduling; its run history stays.",
                action.name
            ),
            AutomationOperation::CancelOccurrence { .. } => {
                "Cancel this run and pause the automation.".to_string()
            }
            AutomationOperation::Save { .. } => String::new(),
        };
        let mut panel = div()
            .id("automation-action")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.action = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .w_full()
            .p(crate::typography::ui_rems(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.separator(false))
            .bg(theme.fill(crate::theme::FillTier::Quaternary))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(6.0))
            .child(widgets::row_title(theme, what));
        let (label, enabled) = match &action.phase {
            Phase::Previewing => {
                panel = panel.child(widgets::row_description(theme, "Checking with the host…"));
                ("Confirm", false)
            }
            Phase::Reviewed => {
                let expired = action
                    .preview
                    .as_ref()
                    .is_none_or(|preview| preview.expires_at_ms <= now_ms());
                if let Some(preview) = &action.preview {
                    panel =
                        panel.child(widgets::row_description(theme, preview.description.clone()));
                }
                if expired {
                    panel = panel.child(
                        widgets::row_description(theme, "This review expired. Review it again.")
                            .text_color(warning),
                    );
                    ("Review again", true)
                } else {
                    ("Confirm", true)
                }
            }
            Phase::Applying => ("Saving…", false),
            Phase::Unknown { checking } => {
                panel = panel.child(
                    widgets::row_description(
                        theme,
                        "LUNA couldn't confirm this change. Check its status; it was not sent again.",
                    )
                    .text_color(warning),
                );
                ("Check status", !checking)
            }
            Phase::Error(error) => {
                panel =
                    panel.child(widgets::row_description(theme, error.clone()).text_color(danger));
                ("Confirm", false)
            }
            Phase::Editing => ("Confirm", false),
        };
        let unknown = matches!(action.phase, Phase::Unknown { .. });
        let error = matches!(action.phase, Phase::Error(_));
        let destructive =
            matches!(action.request.operation, AutomationOperation::Delete) && label == "Confirm";
        let mut buttons = div()
            .w_full()
            .mt(crate::typography::ui_rems(4.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_end()
            .gap(crate::typography::ui_rems(8.0))
            .child(
                crate::button::button(
                    "automation-action-cancel",
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Close")
                .child("Close")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.action = None;
                    cx.notify();
                })),
            );
        if error {
            buttons = buttons.child(
                crate::button::button(
                    "automation-action-reload",
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Reload")
                .child("Reload")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.action = None;
                    this.refresh(true, cx);
                })),
            );
        }
        if unknown {
            buttons = buttons.child(
                crate::button::button(
                    "automation-action-resend",
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Send the same change again")
                .child("Send the same change again")
                .on_click(cx.listener(|this, _, _, cx| this.confirm_action(cx))),
            );
        }
        buttons = buttons.child(
            crate::button::button(
                "automation-action-confirm",
                if destructive {
                    ButtonKind::Danger
                } else {
                    ButtonKind::Primary
                },
                ControlSize::Sm,
                enabled,
                theme,
            )
            .aria_label(label)
            .child(label)
            .when(enabled, |button| {
                button.on_click(cx.listener(move |this, _, _, cx| {
                    if unknown {
                        this.check_action(cx)
                    } else if label == "Review again" {
                        this.review_action(cx)
                    } else {
                        this.confirm_action(cx)
                    }
                }))
            }),
        );
        panel.child(buttons).into_any_element()
    }

    /// An automation's run history: a bordered list (scrolling past its
    /// cap) of runs, each with its state badge and meaning, timing, the
    /// conversation and ids, and its actions; Load more runs closes it.
    fn render_runs(
        &self,
        record: &AutomationRecord,
        runs: &Runs,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let zone = record.definition.timezone.clone();
        let mut list = div()
            .id(SharedString::from(format!("automation-runs-{}", record.id)))
            .w_full()
            .max_h(crate::typography::ui_rems(320.0))
            .overflow_y_scroll()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.separator(false))
            .flex()
            .flex_col();
        let entry = |first: bool| {
            div()
                .w_full()
                .px(crate::typography::ui_rems(12.0))
                .py(crate::typography::ui_rems(10.0))
                .when(!first, |el| {
                    el.border_t_1().border_color(theme.separator(false))
                })
                .flex()
                .flex_col()
                .gap(crate::typography::ui_rems(2.0))
        };
        let mut first = true;
        if let Some(error) = &runs.error {
            list = list.child(
                entry(first).child(
                    widgets::row_description(theme, error.clone())
                        .text_color(theme.readable_tone(theme.danger_muted, 1.0)),
                ),
            );
            first = false;
        }
        let items: Vec<AutomationRun> = runs.items().cloned().collect();
        if items.is_empty() {
            list = list.child(entry(first).child(widgets::row_description(theme, "No runs yet.")));
            first = false;
        }
        for run in items {
            let (state, meaning) = run_state_label(run.state);
            let mut row = entry(first)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .items_center()
                        .gap_x(crate::typography::ui_rems(8.0))
                        .gap_y(crate::typography::ui_rems(2.0))
                        .child(if run.state == AutomationRunState::Completed {
                            widgets::badge_active(theme, state)
                        } else {
                            widgets::badge(theme, state)
                        })
                        .child(widgets::row_description(theme, meaning).text_color(theme.text)),
                )
                .child(widgets::row_description(
                    theme,
                    format!(
                        "Scheduled {} · claimed {}{}",
                        when(run.scheduled_at_ms, &zone),
                        when(run.claimed_at_ms, &zone),
                        if run.coalesced > 0 {
                            format!(" · {} missed slots folded in", run.coalesced)
                        } else {
                            String::new()
                        }
                    ),
                ));
            first = false;
            if !run.detail.trim().is_empty() {
                row = row.child(widgets::row_description(theme, run.detail.clone()));
            }
            row = row.child(widgets::row_description(
                theme,
                format!(
                    "Conversation {} · command {} · message {}",
                    if run.task_id.is_empty() {
                        "—".to_string()
                    } else {
                        self.chat_title(&run.task_id, cx)
                    },
                    if run.command_id.is_empty() {
                        "—"
                    } else {
                        run.command_id.as_str()
                    },
                    if run.message_id.is_empty() {
                        "—"
                    } else {
                        run.message_id.as_str()
                    },
                ),
            ));
            let open =
                (!run.task_id.is_empty()).then(|| {
                    let task = run.task_id.clone();
                    crate::button::button(
                        format!("automation-run-open-{}", run.id),
                        ButtonKind::Ghost,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .aria_label("Open conversation")
                    .child("Open conversation")
                    .child(crate::button::glyph(icons::ARROW_UP_RIGHT, ControlSize::Sm))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(AutomationsEvent::OpenChat(task.clone()))
                    }))
                });
            let cancel = (run.state == AutomationRunState::Claimed).then(|| {
                let record = record.clone();
                let occurrence = run.id.clone();
                crate::button::button(
                    format!("automation-run-cancel-{}", run.id),
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Cancel this run and pause the automation")
                .child("Cancel run and pause")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.begin_action(
                        &record,
                        AutomationOperation::CancelOccurrence {
                            occurrence_id: occurrence.clone(),
                        },
                        cx,
                    )
                }))
            });
            if open.is_some() || cancel.is_some() {
                row = row.child(
                    div()
                        .mt(crate::typography::ui_rems(6.0))
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(crate::typography::ui_rems(8.0))
                        .children(open)
                        .children(cancel),
                );
            }
            list = list.child(row);
        }
        if runs.next().is_some() {
            let id = record.id.clone();
            let enabled = !runs.loading_more;
            list = list.child(
                entry(first).items_start().child(
                    crate::button::button(
                        format!("automation-runs-more-{}", record.id),
                        ButtonKind::Ghost,
                        ControlSize::Sm,
                        enabled,
                        theme,
                    )
                    .aria_label("Load more runs")
                    .child(if enabled {
                        "Load more runs"
                    } else {
                        "Loading…"
                    })
                    .when(enabled, |button| {
                        button.on_click(
                            cx.listener(move |this, _, _, cx| this.load_runs(&id, true, cx)),
                        )
                    }),
                ),
            );
        }
        list.into_any_element()
    }

    /// The create/edit form as a section card: a header (title, the source
    /// conversation, Close), one row per field group with its inputs in a
    /// full-width block, the host's review, and Review / Apply at the end.
    fn render_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(editor) = self.editor.as_ref() else {
            return div().id("automation-editor");
        };
        let warning = theme.readable_tone(theme.warning, 1.0);
        let danger = theme.readable_tone(theme.danger_muted, 1.0);
        // A labelled input: a caption over the field; fields share a row
        // and wrap below their minimum width.
        let field = |label: &'static str, input: &Entity<ComposerInput>| {
            div()
                .flex_1()
                .min_w(crate::typography::ui_rems(160.0))
                .flex()
                .flex_col()
                .gap(crate::typography::ui_rems(4.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .line_height(crate::typography::ui_rems(14.0))
                        .text_color(theme.readable_muted(0.85))
                        .child(label),
                )
                .child(input_frame(input, theme))
        };
        let choice = |id: String, label: &'static str, on: bool, role: gpui::Role| {
            crate::button::chip(id, on, ControlSize::Sm, true, theme)
                .role(role)
                .aria_label(label)
                .child(label)
        };
        let close = crate::button::button(
            "automation-editor-close",
            ButtonKind::Ghost,
            ControlSize::Sm,
            true,
            theme,
        )
        .aria_label("Close")
        .child("Close")
        .on_click(cx.listener(|this, _, _, cx| {
            this.editor = None;
            this.flow_task = None;
            cx.notify();
        }));
        let mut form = widgets::section_card(theme)
            .id("automation-editor")
            // Escape closes the editor here; it never reaches the composer
            // or a stop path behind it.
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.editor = None;
                    this.flow_task = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(widgets::section_header(
                theme,
                if editor.existing {
                    "Edit automation"
                } else {
                    "New automation"
                },
                Some(
                    format!(
                        "From {} (its project, folder, provider and access are captured when saved).",
                        self.chat_title(&editor.origin, cx)
                    )
                    .into(),
                ),
                vec![close.into_any_element()],
            ))
            .child(
                widgets::card_row(theme, false)
                    .child(widgets::row_block().mt(px(0.0)).child(field("Name", &editor.name)))
                    .child(
                        widgets::row_block()
                            .mt(px(0.0))
                            .child(field("What to send", &editor.prompt)),
                    ),
            );
        // Target.
        let mut targets = widgets::row_block();
        for (id, label, new_conversation) in [
            ("automation-target-existing", "This conversation", false),
            ("automation-target-new", "A new conversation each run", true),
        ] {
            targets = targets.child(
                choice(
                    id.into(),
                    label,
                    editor.new_conversation == new_conversation,
                    gpui::Role::RadioButton,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editor) = this.editor.as_mut() {
                        editor.new_conversation = new_conversation;
                    }
                    cx.notify();
                })),
            );
        }
        form = form.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(theme, "Sends to", None))
                .child(targets),
        );
        // Trigger kind and its fields.
        let mut kinds = widgets::row_block();
        for kind in [
            TriggerKind::Once,
            TriggerKind::Interval,
            TriggerKind::Calendar,
            TriggerKind::PrWatch,
        ] {
            kinds = kinds.child(
                choice(
                    format!("automation-kind-{}", kind.label()),
                    kind.label(),
                    editor.kind == kind,
                    gpui::Role::RadioButton,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editor) = this.editor.as_mut() {
                        editor.kind = kind;
                    }
                    cx.notify();
                })),
            );
        }
        let mut schedule = widgets::card_row(theme, false)
            .child(widgets::row_text(theme, "Schedule", None))
            .child(kinds);
        match editor.kind {
            TriggerKind::Once => {
                schedule = schedule.child(
                    widgets::row_block()
                        .child(field("Date (YYYY-MM-DD)", &editor.date))
                        .child(field("Time (HH:MM)", &editor.time))
                        .child(field("Time zone", &editor.timezone)),
                );
            }
            TriggerKind::Interval => {
                schedule = schedule
                    .child(widgets::row_block().child(field("Every (minutes)", &editor.every)));
            }
            TriggerKind::Calendar => {
                let mut days = widgets::row_block()
                    .gap(crate::typography::ui_rems(6.0))
                    .child(
                        div()
                            .w_full()
                            .text_size(crate::typography::ui_rems(11.0))
                            .line_height(crate::typography::ui_rems(14.0))
                            .text_color(theme.readable_muted(0.85))
                            .child("Days (none chosen: every day)"),
                    );
                for (index, day) in WEEKDAYS.into_iter().enumerate() {
                    days = days.child(
                        choice(
                            format!("automation-day-{index}"),
                            day,
                            editor.weekdays[index],
                            gpui::Role::CheckBox,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.editor.as_mut() {
                                editor.weekdays[index] = !editor.weekdays[index];
                            }
                            cx.notify();
                        })),
                    );
                }
                schedule = schedule
                    .child(
                        widgets::row_block()
                            .child(field("Time (HH:MM)", &editor.time))
                            .child(field("Time zone", &editor.timezone)),
                    )
                    .child(days);
            }
            TriggerKind::PrWatch => {
                schedule = schedule
                    .child(
                        widgets::row_block()
                            .child(field("Repository (owner/repository)", &editor.repository))
                            .child(field("Pull request number", &editor.number)),
                    )
                    .child(
                        widgets::row_block()
                            .child(field("Check every (minutes, 1-1440)", &editor.poll))
                            .child(field("Wake budget (1-100)", &editor.budget)),
                    )
                    .child(widgets::row_block().child(widgets::row_description(
                        theme,
                        "Wakes only on meaningful changes. LUNA never posts GitHub comments.",
                    )));
            }
        }
        form =
            form.child(schedule)
                .child(widgets::card_row(theme, false).child(
                    widgets::row_block().mt(px(0.0)).child(field(
                        "Skip runs more than this many minutes late (0-1440)",
                        &editor.lateness,
                    )),
                ));
        // Review: the host's answer for the exact request, then the actions.
        let mut review = div()
            .flex_1()
            .min_w(crate::typography::ui_rems(widgets::ROW_TEXT_MIN))
            .flex()
            .flex_col()
            .gap(crate::typography::ui_rems(2.0));
        if editor.resumes {
            review = review.child(widgets::row_description(
                theme,
                "This automation is paused. Applying saves the change and resumes it.",
            ));
        }
        let current = self.editor_preview_current(cx);
        match &editor.phase {
            Phase::Previewing => {
                review = review.child(widgets::row_description(
                    theme,
                    "Asking the host to review…",
                ))
            }
            Phase::Reviewed if current && self.editor_preview_expired() => {
                review = review.child(
                    widgets::row_description(
                        theme,
                        "This review expired. Review it again before applying.",
                    )
                    .text_color(warning),
                );
            }
            Phase::Reviewed if current => {
                if let Some((_, preview)) = &editor.preview {
                    let zone = editor.timezone.read(cx).text().trim().to_string();
                    review =
                        review.child(widgets::row_description(theme, preview.description.clone()));
                    for at in &preview.next_occurrences_ms {
                        review = review.child(
                            widgets::row_description(theme, format!("Next: {}", when(*at, &zone)))
                                .text_color(theme.text),
                        );
                    }
                    review = review.child(widgets::row_description(
                        theme,
                        format!(
                            "Runs on {} with access up to {}.",
                            self.host_name(&preview.owner_host_id, cx),
                            crate::execution_view::permission_title(preview.permission_ceiling)
                        ),
                    ));
                }
            }
            Phase::Reviewed => {
                review = review.child(
                    widgets::row_description(
                        theme,
                        "You changed the form after review. Review it again.",
                    )
                    .text_color(warning),
                );
            }
            Phase::Applying => review = review.child(widgets::row_description(theme, "Saving…")),
            Phase::Unknown { checking } => {
                review = review.child(
                    widgets::row_description(
                        theme,
                        if *checking {
                            "Checking its status…"
                        } else {
                            "LUNA couldn't confirm this change. Check its status; it was not sent again."
                        },
                    )
                    .text_color(warning),
                );
            }
            Phase::Error(error) => {
                review =
                    review.child(widgets::row_description(theme, error.clone()).text_color(danger));
            }
            Phase::Editing => {
                review = review.child(widgets::row_description(
                    theme,
                    "Review asks the host to check the change before you apply it.",
                ));
            }
        }
        let unknown = matches!(editor.phase, Phase::Unknown { .. });
        let checking = matches!(editor.phase, Phase::Unknown { checking: true });
        let busy = matches!(editor.phase, Phase::Previewing | Phase::Applying) || unknown;
        let button = |id: &'static str, label: &'static str, kind: ButtonKind, enabled: bool| {
            crate::button::button(id, kind, ControlSize::Md, enabled, theme)
                .aria_label(label)
                .child(label)
        };
        let mut buttons = widgets::row_controls();
        if matches!(editor.phase, Phase::Error(_)) {
            buttons = buttons.child(
                button(
                    "automation-editor-reload",
                    "Reload",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.refresh(true, cx))),
            );
        }
        let review_enabled = !busy;
        buttons = buttons.child(
            button(
                "automation-editor-review",
                "Review",
                ButtonKind::Secondary,
                review_enabled,
            )
            .when(review_enabled, |review| {
                review.on_click(cx.listener(|this, _, _, cx| this.review_editor(cx)))
            }),
        );
        if unknown {
            buttons = buttons
                .child(
                    button(
                        "automation-editor-check",
                        "Check status",
                        ButtonKind::Secondary,
                        !checking,
                    )
                    .when(!checking, |check| {
                        check.on_click(cx.listener(|this, _, _, cx| this.check_editor(cx)))
                    }),
                )
                .child(
                    button(
                        "automation-editor-resend",
                        "Send the same change again",
                        ButtonKind::Primary,
                        !checking,
                    )
                    .when(!checking, |again| {
                        again.on_click(cx.listener(|this, _, _, cx| this.apply_editor(cx)))
                    }),
                );
        } else {
            let apply_enabled = matches!(editor.phase, Phase::Reviewed)
                && current
                && !self.editor_preview_expired();
            buttons = buttons.child(
                button(
                    "automation-editor-apply",
                    "Apply",
                    ButtonKind::Primary,
                    apply_enabled,
                )
                .when(apply_enabled, |apply| {
                    apply.on_click(cx.listener(|this, _, _, cx| this.apply_editor(cx)))
                }),
            );
        }
        form.child(widgets::card_row(theme, false).child(review).child(buttons))
    }
}

/// Headless QA drives the panel through its own paths.
#[cfg(feature = "appshots-fixture")]
impl AutomationsPanel {
    /// Follow another conversation (`None`: app-wide).
    pub fn fixture_scope(&mut self, chat: Option<&str>, cx: &mut Context<Self>) {
        let scope = match chat {
            Some(chat) => AutomationScope::Conversation(chat.to_string()),
            None => AutomationScope::All,
        };
        self.set_scope(scope, cx);
    }
    pub fn fixture_more(&mut self, cx: &mut Context<Self>) {
        self.load_more(cx);
    }
    pub fn fixture_new(&mut self, cx: &mut Context<Self>) {
        self.open_editor(None, cx);
    }
    pub fn fixture_edit(&mut self, id: &str, cx: &mut Context<Self>) {
        let record = self.rows().find(|record| record.id == id).cloned();
        if let Some(record) = record {
            self.open_editor(Some(record), cx);
        }
    }
    /// Set a field: name, prompt, timezone, date, time, every, lateness,
    /// repository, number, poll, budget; or kind (once/interval/calendar/pr),
    /// target (existing/new), day (1-7 toggles).
    pub fn fixture_set(&mut self, field: &str, value: &str, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let input = match field {
            "name" => Some(editor.name.clone()),
            "prompt" => Some(editor.prompt.clone()),
            "timezone" => Some(editor.timezone.clone()),
            "date" => Some(editor.date.clone()),
            "time" => Some(editor.time.clone()),
            "every" => Some(editor.every.clone()),
            "lateness" => Some(editor.lateness.clone()),
            "repository" => Some(editor.repository.clone()),
            "number" => Some(editor.number.clone()),
            "poll" => Some(editor.poll.clone()),
            "budget" => Some(editor.budget.clone()),
            "kind" => {
                editor.kind = match value {
                    "interval" => TriggerKind::Interval,
                    "calendar" => TriggerKind::Calendar,
                    "pr" => TriggerKind::PrWatch,
                    _ => TriggerKind::Once,
                };
                None
            }
            "target" => {
                editor.new_conversation = value == "new";
                None
            }
            "day" => {
                if let Ok(day) = value.parse::<usize>()
                    && (1..=7).contains(&day)
                {
                    editor.weekdays[day - 1] = !editor.weekdays[day - 1];
                }
                None
            }
            _ => None,
        };
        if let Some(input) = input {
            let value = value.to_string();
            input.update(cx, |input, cx| input.set_text(value, cx));
        }
        cx.notify();
    }
    pub fn fixture_review(&mut self, cx: &mut Context<Self>) {
        self.review_editor(cx);
    }
    /// Apply, only when the button is enabled.
    pub fn fixture_apply(&mut self, cx: &mut Context<Self>) -> bool {
        let enabled = matches!(
            self.editor.as_ref().map(|editor| &editor.phase),
            Some(Phase::Reviewed)
        ) && self.editor_preview_current(cx)
            && !self.editor_preview_expired();
        if enabled {
            self.apply_editor(cx);
        }
        enabled
    }
    pub fn fixture_check(&mut self, cx: &mut Context<Self>) {
        self.check_editor(cx);
        self.check_action(cx);
    }
    pub fn fixture_lose_next_mutation(&mut self) {
        self.lose_next_mutation = true;
    }
    pub fn fixture_action(&mut self, id: &str, op: &str, cx: &mut Context<Self>) {
        let Some(record) = self.rows().find(|record| record.id == id).cloned() else {
            return;
        };
        let operation = match op {
            "pause" => AutomationOperation::Pause,
            "resume" => AutomationOperation::Resume,
            "delete" => AutomationOperation::Delete,
            other => AutomationOperation::CancelOccurrence {
                occurrence_id: other.trim_start_matches("cancel:").to_string(),
            },
        };
        self.begin_action(&record, operation, cx);
    }
    /// Activate a row's enable control through the button's own path;
    /// returns the control it offered ("Pause", "Resume" or "Edit time").
    pub fn fixture_toggle(&mut self, id: &str, cx: &mut Context<Self>) -> Option<&'static str> {
        let record = self.rows().find(|record| record.id == id).cloned();
        record.map(|record| self.activate_toggle(&record, cx).label())
    }
    pub fn fixture_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirm_action(cx);
    }
    pub fn fixture_toggle_runs(&mut self, id: &str, cx: &mut Context<Self>) {
        self.toggle_runs(id.to_string(), cx);
    }
    pub fn fixture_more_runs(&mut self, id: &str, cx: &mut Context<Self>) {
        self.load_runs(id, true, cx);
    }
    pub fn fixture_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh(true, cx);
    }
    pub fn fixture_readback(&self, cx: &App) -> serde_json::Value {
        let phase = |phase: &Phase| match phase {
            Phase::Editing => "editing".to_string(),
            Phase::Previewing => "previewing".to_string(),
            Phase::Reviewed => "reviewed".to_string(),
            Phase::Applying => "applying".to_string(),
            Phase::Unknown { checking } => {
                format!("unknown{}", if *checking { ":checking" } else { "" })
            }
            Phase::Error(error) => format!("error:{error}"),
        };
        serde_json::json!({
            "scope": match &self.scope {
                AutomationScope::Conversation(chat) => serde_json::json!({"conversation": chat}),
                AutomationScope::All => serde_json::json!("all"),
            },
            "foreignHost": self.foreign_host(cx),
            "loading": self.loading,
            "error": self.error,
            "rows": self.rows().map(|record| serde_json::json!({
                "id": record.id,
                "name": record.definition.name,
                "revision": record.revision,
                "enabled": record.enabled,
                "deleted": record.deleted,
                "status": Self::status_line(record),
                "toggle": RowToggle::for_record(record, now_ms()).label(),
                "trigger": describe_trigger(&record.definition),
                "nextDueMs": record.next_due_ms,
                "target": record.definition.target,
            })).collect::<Vec<_>>(),
            "next": self.rows_next(),
            "runs": self.runs.iter().map(|(id, runs)| (id.clone(), serde_json::json!({
                "items": runs.items().map(|run| serde_json::json!({
                    "id": run.id,
                    "state": run_state_label(run.state).0,
                    "scheduledAtMs": run.scheduled_at_ms,
                    "claimedAtMs": run.claimed_at_ms,
                    "coalesced": run.coalesced,
                    "taskId": run.task_id,
                    "commandId": run.command_id,
                    "messageId": run.message_id,
                    "detail": run.detail,
                })).collect::<Vec<_>>(),
                "next": runs.next(),
                "error": runs.error,
            }))).collect::<serde_json::Map<_, _>>(),
            "editor": self.editor.as_ref().map(|editor| serde_json::json!({
                "requestId": editor.request_id,
                "automationId": editor.automation_id,
                "expectedRevision": editor.expected_revision,
                "existing": editor.existing,
                "resumes": editor.resumes,
                "phase": phase(&editor.phase),
                "previewCurrent": self.editor_preview_current(cx),
                "nextOccurrencesMs": editor.preview.as_ref().map(|(_, preview)| preview.next_occurrences_ms.clone()),
                "built": Self::build_definition(editor, cx).err(),
            })),
            "action": self.action.as_ref().map(|action| serde_json::json!({
                "automationId": action.automation_id,
                "requestId": action.request.request_id,
                "operation": action.request.operation,
                "phase": phase(&action.phase),
            })),
        })
    }
}
