//! Host-owned automations and PR watches: the focused conversation's view (a
//! right-pane surface) and the project-scoped manager (the Automations page),
//! one panel type with two scopes. Every change is previewed by the owning
//! host and applied as that exact request; a lost answer is resolved only
//! through its receipt. An automation belongs to a project on this host; the
//! chat that created it is provenance, not where it must run. Claude-native
//! scheduled prompts are a separate observation (the Goal popover) and never
//! appear here.
//!
//! The transcript side lives here too: [`AutomationCards`] keeps the live
//! record behind each inline automation card, and [`SCHEDULED_MESSAGE_PREFIX`]
//! marks the user messages a schedule sent.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, KeyDownEvent, Render,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    AutomationDefinition, AutomationExecution, AutomationMutationReceipt,
    AutomationMutationRequest, AutomationNotificationPolicy, AutomationOperation, AutomationPage,
    AutomationPreview, AutomationReadRequest, AutomationRecord, AutomationRun, AutomationRunState,
    AutomationTarget, AutomationTrigger, HarnessId, ReasoningLevel,
};
use luna_rpc::{RpcError, methods};

use crate::button::{ButtonKind, ControlSize};
use crate::composer::ComposerInput;
use crate::icons;
use crate::pickers::{DraftConfig, Pickers};
use crate::popover::{self, ScrollRailHost};
use crate::settings::widgets;
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;
use crate::typography::ui_rems;

/// One page of definitions or runs (the host's default; its maximum is 100).
const PAGE: usize = 50;
/// The visible focused view re-reads its first page this often.
const POLL: Duration = Duration::from_secs(5);
/// A panel counts as visible while it rendered this recently.
const VISIBLE_FOR: Duration = Duration::from_secs(6);
/// A custom recurrence rule's local length limit (the host validates fields).
const RULE_MAX: usize = 512;
/// Why Review waits: the chosen provider's model catalog hasn't landed.
const MODELS_LOADING: &str = "Loading this provider's models…";

/// The backend-owned identity of a user message a schedule sent (the
/// scheduler mints `automation-message:{occurrence}`); ordinary sends never
/// carry it.
pub const SCHEDULED_MESSAGE_PREFIX: &str = "automation-message:";

/// Whether a transcript message was sent by a schedule, not typed.
pub fn is_scheduled_message(message_id: &str) -> bool {
    message_id.starts_with(SCHEDULED_MESSAGE_PREFIX)
}

/// Which automations a panel shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationScope {
    /// The focused conversation (definitions it created or sends to).
    Conversation(String),
    /// The Automations page: the sidebar's project, or every project on this
    /// host (`None`, each row naming its project).
    Project(Option<String>),
}

/// What a panel asks its shell to do.
pub enum AutomationsEvent {
    /// Open the conversation a run delivered to (related-chat routing).
    OpenChat(String),
}

/// A run's state in words; Accepted is never completion, Unknown never
/// offers a retry, and Resolved never claims delivery.
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
            "Delivery can't be proven either way. Inspect the conversation; it is never resent automatically. Resolve it to let later runs go ahead.",
        ),
        AutomationRunState::Resolved => (
            "Resolved",
            "You acknowledged this uncertain run. It was not resent, and it may or may not have been delivered.",
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

/// How fresh a record is: its revision, then (at one revision, which the
/// scheduler keeps while it advances the next run) when the host last
/// wrote it. A larger value is the later state.
fn freshness(record: &AutomationRecord) -> (u64, i64) {
    (record.revision, record.updated_at_ms)
}

/// This device's time zone, read once.
fn device_zone() -> Option<&'static str> {
    static ZONE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ZONE.get_or_init(luna_engine::automation::system_timezone)
        .as_deref()
}

/// A run time at a glance: the clock alone when it is today in its zone,
/// else the date and clock; how far off it is when within a day; the zone
/// only when it is not this device's ("14:03 · in 2 min").
pub fn when_brief(ms: i64, zone: &str, now: i64) -> String {
    let full = luna_engine::automation::format_in_zone(ms, zone);
    let today = luna_engine::automation::format_in_zone(now, zone);
    let (Some(date), Some("T"), Some(clock)) = (full.get(..10), full.get(10..11), full.get(11..16))
    else {
        return full;
    };
    let mut text = if today.get(..10) == Some(date) {
        clock.to_string()
    } else {
        format!("{date} {clock}")
    };
    if device_zone() != Some(zone) {
        text.push_str(&format!(" {zone}"));
    }
    let ahead = ms - now;
    if (0..86_400_000).contains(&ahead) {
        let minutes = ahead / 60_000;
        text.push_str(&match minutes {
            0 => " · in under a minute".to_string(),
            1..=59 => format!(" · in {minutes} min"),
            _ => format!(" · in {} h", minutes / 60),
        });
    }
    text
}

/// A definition's lifecycle in one word. Deleted wins (its history stays);
/// a one-shot that ran is Finished, never Active or Paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Active,
    Paused,
    Finished,
    Deleted,
}

impl Lifecycle {
    pub fn of(record: &AutomationRecord) -> Self {
        if record.deleted {
            Self::Deleted
        } else if record.completed {
            Self::Finished
        } else if record.enabled {
            Self::Active
        } else {
            Self::Paused
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Paused => "Paused",
            Self::Finished => "Finished",
            Self::Deleted => "Deleted",
        }
    }
}

/// A one-shot whose instant is not after `now`. The host schedules a
/// one-shot only strictly after the current time and refuses to save a past
/// one, so a paused spent one-shot can't simply resume: it needs a new time.
fn one_shot_spent(record: &AutomationRecord, now: i64) -> bool {
    matches!(record.definition.trigger, AutomationTrigger::Once { at_ms } if at_ms <= now)
}

/// A row's enable control: Pause while active; Resume when paused; Edit time
/// for a paused one-shot whose time has passed, or a finished one (it opens
/// this row's editor; a saved future time schedules it again). Paused future
/// one-shots, intervals, calendars, custom rules and PR watches keep Resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowToggle {
    Pause,
    Resume,
    EditTime,
}

impl RowToggle {
    fn for_record(record: &AutomationRecord, now: i64) -> Self {
        match Lifecycle::of(record) {
            Lifecycle::Active => Self::Pause,
            Lifecycle::Finished => Self::EditTime,
            _ if one_shot_spent(record, now) => Self::EditTime,
            _ => Self::Resume,
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
const RRULE_DAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// The cadence in words, in its own zone.
pub fn describe_trigger(definition: &AutomationDefinition) -> String {
    let zone = &definition.timezone;
    match &definition.trigger {
        AutomationTrigger::Once { at_ms } => format!("Once, {}", when(*at_ms, zone)),
        AutomationTrigger::Interval { every_minutes, .. } => match every_minutes {
            1 => "Every minute".into(),
            60 => "Every hour".into(),
            1440 => "Every day".into(),
            minutes if minutes % 1440 == 0 => format!("Every {} days", minutes / 1440),
            minutes if minutes % 60 == 0 => format!("Every {} hours", minutes / 60),
            minutes => format!("Every {minutes} minutes"),
        },
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
        AutomationTrigger::RRule { rule, .. } => {
            describe_rule(rule).unwrap_or_else(|| format!("Custom rule {}", rule.trim()))
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

/// A recurrence rule in words ("Every 2 weeks on Mon, Wed at 09:30"), for
/// the host's supported fields (FREQ, INTERVAL, BYDAY with ordinals,
/// BYMONTHDAY, BYMONTH, BYHOUR, BYMINUTE, WKST, UNTIL); `None` for anything
/// else, so the caller shows the rule itself. The host's preview is
/// authoritative.
pub fn describe_rule(rule: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let body = rule.trim();
    let body = body.strip_prefix("RRULE:").unwrap_or(body);
    let mut freq = None;
    let mut interval = 1u32;
    let mut days = Vec::new();
    let mut month_days = Vec::new();
    let mut months = Vec::new();
    let mut hours = Vec::new();
    let mut minutes = Vec::new();
    let mut until = None;
    for part in body.split(';').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=')?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(value.to_ascii_uppercase()),
            "INTERVAL" => interval = value.parse().ok().filter(|n| *n > 0)?,
            "BYDAY" => {
                for day in value.split(',') {
                    // An optional ordinal: 1MO (first Monday), -1FR (last Friday).
                    let split = day.len().checked_sub(2)?;
                    // Typed text: a split inside a character isn't a day.
                    let (ordinal, code) = (day.get(..split)?, day.get(split..)?);
                    let index = RRULE_DAYS
                        .iter()
                        .position(|known| known.eq_ignore_ascii_case(code))?;
                    let name = WEEKDAYS[index];
                    days.push(match ordinal {
                        "" => name.to_string(),
                        "-1" => format!("last {name}"),
                        ordinal => {
                            let n: i32 = ordinal.trim_start_matches('+').parse().ok()?;
                            match n {
                                1 => format!("first {name}"),
                                2 => format!("second {name}"),
                                3 => format!("third {name}"),
                                4 => format!("fourth {name}"),
                                5 => format!("fifth {name}"),
                                -2 => format!("second-to-last {name}"),
                                _ => return None,
                            }
                        }
                    });
                }
            }
            "BYMONTHDAY" => {
                for day in value.split(',') {
                    let day: i32 = day.parse().ok()?;
                    month_days.push(match day {
                        1..=31 => day.to_string(),
                        -1 => "the last day".to_string(),
                        -31..=-2 => format!("{} days before the end", -day - 1),
                        _ => return None,
                    });
                }
            }
            "BYMONTH" => {
                for month in value.split(',') {
                    let month: usize = month.parse().ok().filter(|m| (1..=12).contains(m))?;
                    months.push(MONTHS[month - 1]);
                }
            }
            "BYHOUR" => {
                for hour in value.split(',') {
                    hours.push(hour.parse::<u32>().ok().filter(|h| *h < 24)?);
                }
            }
            "BYMINUTE" => {
                for minute in value.split(',') {
                    minutes.push(minute.parse::<u32>().ok().filter(|m| *m < 60)?);
                }
            }
            // The week's first day only changes which weeks an INTERVAL counts.
            "WKST" => {
                RRULE_DAYS
                    .iter()
                    .position(|known| known.eq_ignore_ascii_case(value))?;
            }
            "UNTIL" => {
                let date = value.get(..8)?;
                if !date.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                until = Some(format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8]));
            }
            _ => return None,
        }
    }
    let unit = match freq?.as_str() {
        "MINUTELY" => ("minute", "minutes"),
        "HOURLY" => ("hour", "hours"),
        "DAILY" => ("day", "days"),
        "WEEKLY" => ("week", "weeks"),
        "MONTHLY" => ("month", "months"),
        "YEARLY" => ("year", "years"),
        _ => return None,
    };
    let mut text = if interval == 1 {
        format!("Every {}", unit.0)
    } else {
        format!("Every {interval} {}", unit.1)
    };
    if !months.is_empty() {
        text.push_str(&format!(" in {}", months.join(", ")));
    }
    if !days.is_empty() {
        text.push_str(&format!(" on {}", days.join(", ")));
    }
    if !month_days.is_empty() {
        text.push_str(&format!(" on day {}", month_days.join(", ")));
    }
    match (hours.as_slice(), minutes.as_slice()) {
        ([hour], [minute]) => text.push_str(&format!(" at {hour:02}:{minute:02}")),
        ([hour], []) => text.push_str(&format!(" at {hour:02}:00")),
        ([], [minute]) => text.push_str(&format!(" at minute {minute}")),
        ([], []) => {}
        _ => {
            let hours: Vec<String> = hours.iter().map(|h| format!("{h:02}")).collect();
            let minutes: Vec<String> = minutes.iter().map(|m| format!("{m:02}")).collect();
            text.push_str(&format!(
                " at hours {} and minutes {}",
                if hours.is_empty() {
                    "any".into()
                } else {
                    hours.join(", ")
                },
                if minutes.is_empty() {
                    "00".into()
                } else {
                    minutes.join(", ")
                },
            ));
        }
    }
    if let Some(until) = until {
        text.push_str(&format!(", until {until}"));
    }
    Some(text)
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// A calendar date typed as `YYYY-MM-DD`: exactly four, two and two digits
/// naming a real day. Out-of-range parts are refused, never wrapped.
pub fn parse_date(text: &str) -> Result<(i16, i8, i8), String> {
    const FORMAT: &str = "Enter the date as YYYY-MM-DD.";
    let text = text.trim();
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(FORMAT.into());
    };
    let digits =
        |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(year, 4) || !digits(month, 2) || !digits(day, 2) {
        return Err(FORMAT.into());
    }
    let (year, month, day): (i32, u32, u32) = (
        year.parse().map_err(|_| FORMAT.to_string())?,
        month.parse().map_err(|_| FORMAT.to_string())?,
        day.parse().map_err(|_| FORMAT.to_string())?,
    );
    if chrono::NaiveDate::from_ymd_opt(year, month, day).is_none() {
        return Err(format!("{text} is not a calendar date."));
    }
    // Four digits fit i16; a real month and day fit i8.
    Ok((year as i16, month as i8, day as i8))
}

/// A wall-clock time typed as `HH:MM` (one or two hour digits, two minute
/// digits), 00:00 through 23:59.
pub fn parse_clock(text: &str) -> Result<(i8, i8), String> {
    const FORMAT: &str = "Enter the time as HH:MM.";
    let text = text.trim();
    let (hour, minute) = text.split_once(':').ok_or_else(|| FORMAT.to_string())?;
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if hour.is_empty() || hour.len() > 2 || minute.len() != 2 || !digits(hour) || !digits(minute) {
        return Err(FORMAT.into());
    }
    let hour: u8 = hour.parse().map_err(|_| FORMAT.to_string())?;
    let minute: u8 = minute.parse().map_err(|_| FORMAT.to_string())?;
    if hour > 23 || minute > 59 {
        return Err("Enter a time between 00:00 and 23:59.".into());
    }
    Ok((hour as i8, minute as i8))
}

/// The editor's trigger choice ("Repeat").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    Once,
    Interval,
    Calendar,
    Custom,
    PrWatch,
}

impl TriggerKind {
    const ALL: [Self; 5] = [
        Self::Once,
        Self::Interval,
        Self::Calendar,
        Self::Custom,
        Self::PrWatch,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Once => "Once",
            Self::Interval => "Interval",
            Self::Calendar => "Daily or weekly",
            Self::Custom => "Custom",
            Self::PrWatch => "Pull request",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Interval => "interval",
            Self::Calendar => "calendar",
            Self::Custom => "custom",
            Self::PrWatch => "pr",
        }
    }
}

/// Notification choices, in menu order.
const NOTIFICATION_POLICIES: [AutomationNotificationPolicy; 3] = [
    AutomationNotificationPolicy::AllRuns,
    AutomationNotificationPolicy::ImportantUpdates,
    AutomationNotificationPolicy::Never,
];

pub fn notification_label(policy: AutomationNotificationPolicy) -> &'static str {
    match policy {
        AutomationNotificationPolicy::AllRuns => "All runs",
        AutomationNotificationPolicy::ImportantUpdates => "Important updates",
        AutomationNotificationPolicy::Never => "Never",
    }
}

fn notification_caption(policy: AutomationNotificationPolicy) -> &'static str {
    match policy {
        AutomationNotificationPolicy::AllRuns => "Notify when every run finishes",
        AutomationNotificationPolicy::ImportantUpdates => "Failures, uncertain runs and attention",
        AutomationNotificationPolicy::Never => "Runs stay quiet; history still records them",
    }
}

fn notification_key(policy: AutomationNotificationPolicy) -> &'static str {
    match policy {
        AutomationNotificationPolicy::AllRuns => "all",
        AutomationNotificationPolicy::ImportantUpdates => "important",
        AutomationNotificationPolicy::Never => "never",
    }
}

/// Provider, model and reasoning in words ("Codex · gpt-5 · High").
pub fn execution_label(
    harness: HarnessId,
    model: Option<&str>,
    reasoning: Option<ReasoningLevel>,
) -> String {
    let mut parts = vec![crate::shell::board::chat_badges::harness_label(harness).to_string()];
    parts.push(model.unwrap_or("provider default model").to_string());
    if let Some(level) = reasoning {
        parts.push(crate::pickers::reasoning_label(level).to_string());
    }
    parts.join(" · ")
}

/// The execution a record runs with: its pinned choice, else the config
/// captured when it was saved.
fn record_execution_label(record: &AutomationRecord) -> String {
    match &record.definition.execution {
        Some(execution) => execution_label(
            execution.harness,
            execution.model.as_deref(),
            execution.reasoning,
        ),
        None => {
            let config = &record.authority.config;
            execution_label(config.harness, config.model.as_deref(), config.reasoning)
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

/// The editor's drop-down menus (one open at a time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Project,
    Chat,
    Notifications,
}

/// The create/edit form. The request id and automation id are fixed for
/// the dialog; a preview belongs to the exact request it was made for.
struct Editor {
    automation_id: String,
    request_id: String,
    expected_revision: u64,
    existing: bool,
    /// The saved automation is paused: Save keeps it paused (Resume is its
    /// own reviewed action).
    paused: bool,
    kind: TriggerKind,
    weekdays: [bool; 7],
    name: Entity<ComposerInput>,
    prompt: Entity<ComposerInput>,
    timezone: Entity<ComposerInput>,
    date: Entity<ComposerInput>,
    time: Entity<ComposerInput>,
    every: Entity<ComposerInput>,
    lateness: Entity<ComposerInput>,
    rule: Entity<ComposerInput>,
    repository: Entity<ComposerInput>,
    number: Entity<ComposerInput>,
    poll: Entity<ComposerInput>,
    budget: Entity<ComposerInput>,
    /// The project each run belongs to (a project on this host).
    project_id: Option<String>,
    /// A fresh chat in `project_id` each run; else `chat_id`.
    fresh: bool,
    /// The existing chat each run is sent to (in `project_id`).
    chat_id: Option<String>,
    /// The chat that created the automation (provenance; never edited).
    source_task_id: Option<String>,
    /// A saved fresh-chat target's origin, kept while it is in the project.
    origin_task_id: Option<String>,
    notification_policy: AutomationNotificationPolicy,
    /// Archive a fresh chat after a successful run (fresh chats only).
    archive_successful_runs: bool,
    /// Provider, model and reasoning: the composer's own pickers, detached
    /// and bound to the destination project and its host.
    pickers: Entity<Pickers>,
    /// What the pickers held when the editor opened; a saved automation
    /// keeps its execution unless they changed.
    initial_draft: DraftConfig,
    saved_execution: Option<AutomationExecution>,
    /// The saved destination, kept byte for byte while it is unchanged.
    initial_target: Option<AutomationTarget>,
    _pickers_changed: Subscription,
    /// An interval's phase and a custom rule's start, kept from the record.
    anchor_ms: i64,
    preview: Option<(AutomationMutationRequest, AutomationPreview)>,
    phase: Phase,
    /// "More options" (notifications, archiving, lateness) is expanded; its
    /// collapsed summary still names every value.
    more: bool,
    /// What the form would send when its refusal was shown: once an edit
    /// changes that (another field, kind or model), the refusal is about a
    /// form that no longer exists and the form reads as editable again.
    error_basis: Option<Result<AutomationMutationRequest, String>>,
}

/// Pause / Resume / Delete / Cancel occurrence / Resolve: preview, then
/// confirm.
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
        let known: HashSet<&str> = self.first.iter().map(|run| run.id.as_str()).collect();
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
    /// The right sidebar's Automations place: the place's header names it,
    /// so the pane toolbar shows only the actions.
    embedded: bool,
    engine_generation: u64,
    load_seq: u64,
    first: Vec<AutomationRecord>,
    first_next: Option<String>,
    more: Vec<AutomationRecord>,
    more_next: Option<String>,
    loading: bool,
    loading_more: bool,
    /// Shown again after being hidden: the cached rows may describe
    /// automations another surface (an agent, a card) changed or deleted
    /// meanwhile, so they stay hidden until the fresh first page lands.
    revalidating: bool,
    /// The freshest record seen for each automation in this scope and
    /// connection, from reads and saved changes alike ([`freshness`]): a
    /// read or receipt that answers late with an older state never
    /// replaces it (reads and receipts race).
    latest: HashMap<String, AutomationRecord>,
    error: Option<String>,
    show_deleted: bool,
    runs: HashMap<String, Runs>,
    editor: Option<Editor>,
    action: Option<ActionFlow>,
    /// An automation another surface asked to show (an inline card's Open
    /// or Edit): highlighted, its history open, once its row is loaded.
    focus: Option<(String, bool)>,
    /// The row a card's Open last showed.
    highlight: Option<String>,
    /// A focused automation outside the loaded pages (a later page, or
    /// deleted with Show deleted off), listed after them.
    pinned: Option<AutomationRecord>,
    focus_task: Option<Task<()>>,
    menu: popover::Popup<Menu>,
    menu_cursor: Option<usize>,
    last_render: Cell<Option<Instant>>,
    list_task: Option<Task<()>>,
    more_task: Option<Task<()>>,
    runs_tasks: HashMap<String, Task<()>>,
    flow_task: Option<Task<()>>,
    /// The panel's scroller and its floating rail (page and pane alike).
    scroll: widgets::PageScroll,
    _poll: Task<()>,
    /// The host's change stream: every frame re-reads the first page (and
    /// open histories), whoever made the change.
    _watch: Subscription,
    /// QA: report the next mutation's answer as lost.
    #[cfg(feature = "appshots-fixture")]
    lose_next_mutation: bool,
}

impl EventEmitter<AutomationsEvent> for AutomationsPanel {}

/// The project a chat belongs to.
fn chat_project(state: &AppState, chat: &str) -> Option<String> {
    state
        .chats
        .iter()
        .find(|row| row.id == chat)
        .and_then(|row| row.space_id.clone())
}

/// Projects on this host, in sidebar order: automations run on the host
/// that owns them, so only its projects can be destinations.
fn local_projects(state: &AppState) -> Vec<luna_proto::Space> {
    state
        .spaces
        .iter()
        .filter(|space| {
            state
                .local_device_id
                .as_deref()
                .is_none_or(|local| space.device_id == local)
        })
        .cloned()
        .collect()
}

/// A project's name; a removed or unknown one says so.
pub(crate) fn project_name(state: &AppState, project: &str) -> String {
    if let Some(space) = state.spaces.iter().find(|space| space.id == project) {
        return space.display_name().to_string();
    }
    if let Some(space) = state
        .removed_spaces
        .iter()
        .find(|space| space.id == project)
    {
        return format!("{} (removed)", space.display_name());
    }
    "an unknown project".into()
}

/// A chat's title as the sidebar shows it.
fn chat_name(state: &AppState, chat: &str) -> String {
    state
        .chats
        .iter()
        .find(|row| row.id == chat)
        .and_then(|row| row.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Untitled chat".into())
}

/// The chats of `project` on this host a run can be sent to: unarchived,
/// most recently active first.
fn project_chats(state: &AppState, project: &str) -> Vec<luna_proto::Chat> {
    let mut chats: Vec<luna_proto::Chat> = state
        .chats
        .iter()
        .filter(|row| {
            row.space_id.as_deref() == Some(project)
                && !row.archived
                && state
                    .local_device_id
                    .as_deref()
                    .is_none_or(|local| row.device_id == local)
        })
        .cloned()
        .collect();
    chats.sort_by(|a, b| {
        b.last_message_at
            .unwrap_or(b.created_at)
            .cmp(&a.last_message_at.unwrap_or(a.created_at))
    });
    chats
}

/// Why runs can't use `harness` (and `account`) in an existing chat: a run
/// never switches a chat's provider or account (that is the chat's own
/// provider switch, a context boundary), so an automation sending to it may
/// change only the model and effort. `None` when they match, or when the
/// chat's config isn't known here (the host checks again).
fn chat_provider_mismatch(
    state: &AppState,
    chat: &str,
    harness: HarnessId,
    account: Option<&str>,
) -> Option<String> {
    let config = state
        .chats
        .iter()
        .find(|row| row.id == chat)?
        .config
        .as_ref()?;
    let label = crate::shell::board::chat_badges::harness_label;
    let name = chat_name(state, chat);
    if config.harness != harness {
        return Some(format!(
            "{name} runs on {chat_provider}, and a scheduled run can't switch its provider. \
             Choose a {chat_provider} model, or start each run in a new chat to use {wanted}.",
            chat_provider = label(config.harness),
            wanted = label(harness),
        ));
    }
    match (config.provider_account_id.as_deref(), account) {
        (Some(chat_account), Some(account)) if chat_account != account => Some(format!(
            "{name} uses another {} account, and a scheduled run can't switch it. Choose that \
             account, or start each run in a new chat.",
            label(harness)
        )),
        _ => None,
    }
}

/// A saved execution as the pickers' explicit picks.
fn draft_of(execution: &AutomationExecution) -> DraftConfig {
    DraftConfig {
        harness: Some(execution.harness),
        provider_account_id: execution.provider_account_id.clone(),
        model: execution.model.clone(),
        reasoning: execution.reasoning,
        model_options: execution.model_options.clone(),
        ..DraftConfig::default()
    }
}

/// An inline card's record as last read from this host.
#[derive(Debug, Clone)]
enum CardRecord {
    Loading,
    Loaded(Box<AutomationRecord>),
    /// This host has no such automation (another device's, or never saved).
    Missing,
    Failed(String),
}

/// What an inline automation card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardView {
    pub name: String,
    pub detail: String,
    pub status: Option<&'static str>,
    /// The record is loaded, so Open and Edit lead somewhere.
    pub openable: bool,
}

/// A card's record is re-read after this long (another surface may have
/// paused, edited or finished it).
const CARD_FRESH_FOR: Duration = Duration::from_secs(30);

/// The live records behind the transcript's inline automation cards, shared
/// by every transcript. A card reads its automation on first paint; panel
/// reads and saved changes refresh the same entries, so a card follows
/// Pause, Edit and Finished without its own polling.
pub struct AutomationCards {
    records: HashMap<String, (CardRecord, Instant)>,
    engine_generation: u64,
    /// Bumped by every watch frame: a read started before the latest frame
    /// may describe the state the frame replaced, so its answer is dropped.
    read_generation: u64,
    tasks: HashMap<String, Task<()>>,
    /// The host's change stream: a frame makes every card re-read.
    _watch: Option<Subscription>,
}

struct CardsGlobal(Entity<AutomationCards>);

impl gpui::Global for CardsGlobal {}

impl AutomationCards {
    /// The app's card store, created on first use.
    pub fn global(cx: &mut App) -> Entity<AutomationCards> {
        if let Some(cards) = cx.try_global::<CardsGlobal>() {
            return cards.0.clone();
        }
        let cards = cx.new(|_| AutomationCards {
            records: HashMap::new(),
            engine_generation: 0,
            read_generation: 0,
            tasks: HashMap::new(),
            _watch: None,
        });
        cx.set_global(CardsGlobal(cards.clone()));
        cards
    }

    /// Records a panel just read or saved: cards already showing them update.
    pub fn observe_records(records: &[AutomationRecord], cx: &mut App) {
        let Some(cards) = cx.try_global::<CardsGlobal>().map(|cards| cards.0.clone()) else {
            return;
        };
        cards.update(cx, |cards, cx| {
            let mut changed = false;
            for record in records {
                if let Some((slot, at)) = cards.records.get_mut(&record.id) {
                    // An answer older than what a card shows never replaces it.
                    let older = matches!(slot, CardRecord::Loaded(known) if freshness(known) > freshness(record));
                    let same = matches!(slot, CardRecord::Loaded(known) if **known == *record);
                    if older {
                        continue;
                    }
                    *at = Instant::now();
                    if !same {
                        *slot = CardRecord::Loaded(Box::new(record.clone()));
                        changed = true;
                    }
                }
            }
            if changed {
                cx.notify();
            }
        });
    }

    /// A card's view of `id`, reading it from this host when it is unknown
    /// or stale. The view's size never depends on the answer, so a card
    /// doesn't move when its record lands.
    pub fn view(&mut self, state: &Entity<AppState>, id: &str, cx: &mut Context<Self>) -> CardView {
        let watch = AutomationWatch::global(state, cx);
        if self._watch.is_none() {
            self._watch = Some(cx.observe(&watch, |cards, _, cx| cards.invalidate(cx)));
        }
        let generation = state.read(cx).engine_generation();
        if generation != self.engine_generation {
            self.engine_generation = generation;
            self.records.clear();
            self.tasks.clear();
        }
        let stale = self
            .records
            .get(id)
            .is_none_or(|(_, at)| at.elapsed() >= CARD_FRESH_FOR);
        if stale && !self.tasks.contains_key(id) {
            self.fetch(state, id, cx);
        }
        self.describe(id)
    }

    /// The host's automations changed: every card re-reads on its next
    /// paint (what it shows stays until the answer lands).
    fn invalidate(&mut self, cx: &mut Context<Self>) {
        // Reads in flight started before this change: drop them (dropping
        // the task cancels it) and fence any answer already on its way, so
        // the next paint reads again instead of trusting it for 30 s.
        self.read_generation += 1;
        self.tasks.clear();
        let stale = Instant::now().checked_sub(CARD_FRESH_FOR);
        for (_, at) in self.records.values_mut() {
            match stale {
                Some(stale) => *at = stale,
                None => *at = Instant::now(),
            }
        }
        if stale.is_none() {
            // Too close to the clock's origin to backdate: forget instead.
            self.records.clear();
        }
        cx.notify();
    }

    /// What a card for `id` shows from what is known now (no read).
    fn describe(&self, id: &str) -> CardView {
        match self.records.get(id).map(|(record, _)| record) {
            Some(CardRecord::Loaded(record)) => CardView {
                name: record.definition.name.clone(),
                detail: describe_trigger(&record.definition),
                status: Some(Lifecycle::of(record).label()),
                openable: true,
            },
            Some(CardRecord::Missing) => CardView {
                name: "Automation".into(),
                detail: "Not on this device; it is managed on the host that created it.".into(),
                status: None,
                openable: false,
            },
            Some(CardRecord::Failed(error)) => CardView {
                name: "Automation".into(),
                detail: format!("Couldn't read it: {error}"),
                status: None,
                openable: false,
            },
            Some(CardRecord::Loading) | None => CardView {
                name: "Automation".into(),
                detail: "Loading…".into(),
                status: None,
                openable: false,
            },
        }
    }

    /// The loaded record for `id`, if any (Open and Edit route by it).
    pub fn record(&self, id: &str) -> Option<&AutomationRecord> {
        match self.records.get(id) {
            Some((CardRecord::Loaded(record), _)) => Some(record.as_ref()),
            _ => None,
        }
    }

    fn fetch(&mut self, state: &Entity<AppState>, id: &str, cx: &mut Context<Self>) {
        let Some(engine) = state.read(cx).engine().cloned() else {
            return;
        };
        self.records
            .entry(id.to_string())
            .or_insert_with(|| (CardRecord::Loading, Instant::now()));
        let generation = self.engine_generation;
        let read_generation = self.read_generation;
        let automation = id.to_string();
        let request = AutomationReadRequest {
            automation_id: Some(automation.clone()),
            include_deleted: true,
            limit: Some(1),
            ..AutomationReadRequest::default()
        };
        self.tasks.insert(
            automation.clone(),
            cx.spawn(async move |this, cx| {
                let result = list_page(engine, request).await;
                this.update(cx, |cards, cx| {
                    if cards.engine_generation != generation
                        || cards.read_generation != read_generation
                    {
                        // A newer read owns this card (or will on paint).
                        return;
                    }
                    cards.tasks.remove(&automation);
                    let record = match result {
                        Ok(page) => page
                            .items
                            .into_iter()
                            .find(|record| record.id == automation)
                            .map_or(CardRecord::Missing, |record| {
                                CardRecord::Loaded(Box::new(record))
                            }),
                        Err(error) => CardRecord::Failed(error),
                    };
                    // A panel's saved change may have landed while this read
                    // was in flight: the newer revision stays.
                    let newer_known = matches!(
                        (cards.records.get(&automation), &record),
                        (Some((CardRecord::Loaded(known), _)), CardRecord::Loaded(read))
                            if freshness(known) > freshness(read)
                    );
                    if newer_known {
                        return;
                    }
                    cards.records.insert(automation, (record, Instant::now()));
                    cx.notify();
                })
                .ok();
            }),
        );
    }
}

/// How long a lost automation watch waits before subscribing again.
const WATCH_RETRY: Duration = Duration::from_secs(2);

/// The host's automation change stream (`WatchAutomations`): one
/// subscription per engine connection, shared by every panel and the
/// inline cards. Each frame says the host's automations changed (from any
/// actor: this UI, an agent's tool, the scheduler); readers re-read. A
/// reconnect starts a new stream whose sequence starts over.
pub struct AutomationWatch {
    engine_generation: u64,
    /// The last frame's sequence (`None`: no frame on this connection yet).
    seq: Option<u64>,
    task: Option<Task<()>>,
}

struct WatchGlobal(Entity<AutomationWatch>);

impl gpui::Global for WatchGlobal {}

impl AutomationWatch {
    /// The app's watch, subscribed for the current engine connection.
    pub fn global(state: &Entity<AppState>, cx: &mut App) -> Entity<AutomationWatch> {
        let watch = match cx.try_global::<WatchGlobal>() {
            Some(watch) => watch.0.clone(),
            None => {
                let watch = cx.new(|_| AutomationWatch {
                    engine_generation: 0,
                    seq: None,
                    task: None,
                });
                cx.set_global(WatchGlobal(watch.clone()));
                watch
            }
        };
        watch.update(cx, |watch, cx| watch.ensure(state, cx));
        watch
    }

    pub fn seq(&self) -> Option<u64> {
        self.seq
    }

    /// Subscribe for the current connection (once; a reconnect replaces
    /// the stream). An engine without the method leaves the watch idle, and
    /// panels keep their visible-only polling.
    fn ensure(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        let (generation, engine) = {
            let state = state.read(cx);
            (state.engine_generation(), state.engine().cloned())
        };
        if generation == self.engine_generation && self.task.is_some() {
            return;
        }
        let Some(engine) = engine else {
            return;
        };
        self.engine_generation = generation;
        self.seq = None;
        let state = state.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            loop {
                match engine
                    .client()
                    .subscribe(methods::WATCH_AUTOMATIONS, serde_json::json!({}))
                    .await
                {
                    Ok(mut frames) => {
                        while let Some(value) = frames.recv().await {
                            let Ok(frame) =
                                serde_json::from_value::<luna_proto::AutomationWatchFrame>(value)
                            else {
                                tracing::warn!("dropping malformed automation watch frame");
                                continue;
                            };
                            let alive = this.update(cx, |watch, cx| {
                                if watch.engine_generation == generation {
                                    watch.seq = Some(frame.seq);
                                    cx.notify();
                                }
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                    }
                    Err(RpcError::Transport(_) | RpcError::Closed) => {}
                    Err(error) => {
                        tracing::debug!(%error, "automation watch unavailable");
                        return;
                    }
                }
                cx.background_executor().timer(WATCH_RETRY).await;
                // The stream ended: subscribe again on the same connection;
                // a new connection gets its own stream from `ensure`.
                let current = this
                    .update(cx, |watch, cx| {
                        watch.engine_generation == generation
                            && state.read(cx).engine_generation() == generation
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
            }
        }));
    }
}

/// The inline card a chat shows for an automation an agent saved (the
/// reference's "Random number every minute · Every minute · Open"): a clock
/// tile, the name, the cadence and status, and Open / Edit. `on_open`
/// receives `true` for Edit.
pub fn render_card(
    id: SharedString,
    view: &CardView,
    theme: &Theme,
    on_open: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_open = std::rc::Rc::new(on_open);
    let detail = match view.status {
        Some(status) => format!("{} · {status}", view.detail),
        None => view.detail.clone(),
    };
    let action = |key: &'static str, label: &'static str, edit: bool| {
        let on_open = on_open.clone();
        crate::button::button(
            SharedString::from(format!("{id}-{key}")),
            if edit {
                ButtonKind::Ghost
            } else {
                ButtonKind::Secondary
            },
            ControlSize::Sm,
            view.openable,
            theme,
        )
        .aria_label(format!("{label} automation {}", view.name))
        .child(label)
        .when(view.openable, |button| {
            button.on_click(move |_, window, cx| {
                cx.stop_propagation();
                on_open(edit, window, cx)
            })
        })
    };
    div()
        .id(id.clone())
        .role(gpui::Role::Group)
        .aria_label(format!("Automation {} · {detail}", view.name))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(ui_rems(10.0))
        .px(ui_rems(12.0))
        .py(ui_rems(10.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.separator(false))
        .bg(theme.fill(crate::theme::FillTier::Quaternary))
        .child(widgets::row_tile(theme, icons::STOPWATCH_PLAY))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(ui_rems(2.0))
                .child(
                    div()
                        .truncate()
                        .text_size(ui_rems(13.0))
                        .line_height(ui_rems(18.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(view.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(ui_rems(12.0))
                        .line_height(ui_rems(16.0))
                        .text_color(theme.readable_muted(0.85))
                        .child(detail),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .gap(ui_rems(6.0))
                .child(action("edit", "Edit", true))
                .child(action("open", "Open", false)),
        )
        .into_any_element()
}

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
        let watch = AutomationWatch::global(&state, cx);
        let watched = cx.observe(&watch, |panel, _, cx| panel.on_watch(cx));
        let mut panel = Self {
            state,
            scope,
            embedded: false,
            engine_generation: 0,
            load_seq: 0,
            first: Vec::new(),
            first_next: None,
            more: Vec::new(),
            more_next: None,
            loading: false,
            loading_more: false,
            revalidating: false,
            latest: HashMap::new(),
            error: None,
            show_deleted: false,
            runs: HashMap::new(),
            editor: None,
            action: None,
            focus: None,
            highlight: None,
            pinned: None,
            focus_task: None,
            menu: popover::Popup::default(),
            menu_cursor: None,
            last_render: Cell::new(None),
            list_task: None,
            more_task: None,
            runs_tasks: HashMap::new(),
            flow_task: None,
            scroll: widgets::PageScroll::default(),
            _poll: poll,
            _watch: watched,
            #[cfg(feature = "appshots-fixture")]
            lose_next_mutation: false,
        };
        panel.refresh(true, cx);
        panel
    }

    /// Follow the focused conversation or the sidebar's project. A new
    /// scope starts over: nothing from the previous one is kept, so another
    /// project's rows or editor never stand in for this one's.
    pub fn set_scope(&mut self, scope: AutomationScope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.editor = None;
        self.action = None;
        self.flow_task = None;
        self.focus = None;
        self.highlight = None;
        self.pinned = None;
        self.focus_task = None;
        self.close_menu(cx);
        self.refresh(true, cx);
    }

    fn scope_chat(&self) -> Option<&str> {
        match &self.scope {
            AutomationScope::Conversation(chat) => Some(chat),
            AutomationScope::Project(_) => None,
        }
    }

    /// The page's project (`None`: every project, or the conversation view).
    fn scope_project(&self) -> Option<&str> {
        match &self.scope {
            AutomationScope::Project(project) => project.as_deref(),
            AutomationScope::Conversation(_) => None,
        }
    }

    /// The read filter for this scope's definitions.
    fn read_request(&self, after_id: Option<String>) -> AutomationReadRequest {
        AutomationReadRequest {
            automation_id: None,
            task_id: self.scope_chat().map(str::to_string),
            project_id: self.scope_project().map(str::to_string),
            after_id,
            limit: Some(PAGE),
            include_deleted: self.show_deleted,
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
            self.pinned = None;
            self.latest.clear();
        }
        self.engine_generation = generation;
        if self.foreign_host(cx).is_some() {
            self.loading = false;
            self.revalidating = false;
            cx.notify();
            return;
        }
        let Some(engine) = self.engine(cx) else {
            self.error = Some("Engine not connected.".into());
            self.revalidating = false;
            cx.notify();
            return;
        };
        self.load_seq += 1;
        let seq = self.load_seq;
        let scope = self.scope.clone();
        let request = self.read_request(None);
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
                panel.revalidating = false;
                if panel.scope != scope || panel.state.read(cx).engine_generation() != generation {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(page) => {
                        let items = panel.newest(page.items);
                        AutomationCards::observe_records(&items, cx);
                        panel.first = items;
                        panel.first_next = page.next_after_id;
                        panel.error = None;
                        panel.apply_focus(cx);
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
        let known: HashSet<&str> = self.first.iter().map(|record| record.id.as_str()).collect();
        let listed = self.pinned.as_ref().is_some_and(|pinned| {
            self.first
                .iter()
                .chain(self.more.iter())
                .any(|record| record.id == pinned.id)
        });
        self.first
            .iter()
            .chain(
                self.more
                    .iter()
                    .filter(move |record| !known.contains(record.id.as_str())),
            )
            .chain(self.pinned.iter().filter(move |_| !listed))
    }

    /// A page as it may be shown: a record older than the freshest state
    /// already seen (an applied change, or an earlier read the scheduler
    /// had advanced further) is replaced by that state, and a deletion
    /// stays gone while deleted rows are hidden. An equally fresh read
    /// wins and becomes the floor.
    fn newest(&mut self, items: Vec<AutomationRecord>) -> Vec<AutomationRecord> {
        let show_deleted = self.show_deleted;
        let latest = &mut self.latest;
        items
            .into_iter()
            .map(|record| {
                let fresher = latest
                    .get(&record.id)
                    .filter(|known| freshness(known) > freshness(&record))
                    .cloned();
                match fresher {
                    Some(known) => known,
                    None => {
                        latest.insert(record.id.clone(), record.clone());
                        record
                    }
                }
            })
            .filter(|record| show_deleted || !record.deleted)
            .collect()
    }

    /// A change this panel applied: its row (or the pinned one) shows the
    /// receipt's record at once, a deletion leaves the list unless deleted
    /// rows are shown, and no older read can bring the previous state back.
    fn apply_receipt(&mut self, record: &AutomationRecord) {
        // A receipt only moves state forward: a read (or receipt) at least
        // as fresh already describes it, maybe with a later next run.
        if self
            .latest
            .get(&record.id)
            .is_some_and(|known| freshness(known) >= freshness(record))
        {
            return;
        }
        self.latest.insert(record.id.clone(), record.clone());
        let hide = record.deleted && !self.show_deleted;
        for rows in [&mut self.first, &mut self.more] {
            if hide {
                rows.retain(|row| row.id != record.id);
            } else if let Some(row) = rows.iter_mut().find(|row| row.id == record.id) {
                *row = record.clone();
            }
        }
        if let Some(pinned) = self.pinned.as_mut()
            && pinned.id == record.id
        {
            *pinned = record.clone();
        }
    }

    /// The host's automations changed. The first page and open histories
    /// re-read; later pages are dropped (a deleted or changed row there
    /// would otherwise stay) and Load more reads them again.
    fn on_watch(&mut self, cx: &mut Context<Self>) {
        if !self.more.is_empty() {
            self.more.clear();
            self.more_next = None;
            self.more_task = None;
            self.loading_more = false;
        }
        self.refresh(false, cx);
    }

    /// The panel is shown again after its surface was hidden: rows read
    /// before then are not shown until the first page is read again.
    pub fn reveal(&mut self, cx: &mut Context<Self>) {
        if self.foreign_host(cx).is_some() {
            return;
        }
        self.revalidating = !self.first.is_empty() || !self.more.is_empty();
        self.refresh(false, cx);
    }

    /// The rows the list shows: none while it revalidates after a reveal.
    fn shown_rows(&self) -> impl Iterator<Item = &AutomationRecord> {
        self.rows().filter(|_| !self.revalidating)
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
        let request = self.read_request(Some(after.clone()));
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
                        let items = panel.newest(page.items);
                        AutomationCards::observe_records(&items, cx);
                        panel.more.extend(items);
                        panel.more_next = page.next_after_id;
                        panel.apply_focus(cx);
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
        // A deleted automation's history stays readable.
        let request = AutomationReadRequest {
            automation_id: Some(automation.clone()),
            after_id: after.clone(),
            limit: Some(PAGE),
            include_deleted: true,
            ..AutomationReadRequest::default()
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

    /// Detached pickers for the destination project: the composer's model,
    /// reasoning and option chips over that project's host and catalogs,
    /// showing `draft` as explicit picks.
    fn make_pickers(
        state: &Entity<AppState>,
        project: &str,
        draft: DraftConfig,
        cx: &mut Context<Self>,
    ) -> (Entity<Pickers>, Subscription) {
        let state = state.clone();
        let project = project.to_string();
        let pickers = cx.new(move |cx| {
            let mut pickers = Pickers::new_detached(state, project, false, false, cx);
            pickers.set_detached_draft(draft, false, cx);
            pickers
        });
        // A pick changes the request: the form re-renders (and a review of
        // the old request stops being current).
        let changed = cx.observe(&pickers, |_, _, cx| cx.notify());
        (pickers, changed)
    }

    /// Open the editor: a new automation (for the focused conversation, or
    /// the page's project), or an existing one (its own fields, revision and
    /// saved execution).
    fn open_editor(&mut self, existing: Option<AutomationRecord>, cx: &mut Context<Self>) {
        if let Some(host) = self.foreign_host(cx) {
            self.error = Some(format!(
                "This conversation's automations are managed on {host}; LUNA must be open there."
            ));
            cx.notify();
            return;
        }
        self.close_menu(cx);
        let definition = existing.as_ref().map(|record| record.definition.clone());
        let (project_id, fresh, chat_id, source_task_id, origin_task_id) = {
            let state = self.state.read(cx);
            match (&existing, &self.scope) {
                (Some(record), _) => {
                    let definition = &record.definition;
                    let saved_project = Some(record.authority.project_id.clone())
                        .filter(|project| !project.is_empty());
                    match &definition.target {
                        AutomationTarget::Existing { task_id } => (
                            chat_project(state, task_id).or(saved_project),
                            false,
                            Some(task_id.clone()),
                            definition.source_task_id.clone(),
                            None,
                        ),
                        AutomationTarget::NewConversation {
                            origin_task_id,
                            project_id,
                        } => (
                            project_id
                                .clone()
                                .or_else(|| {
                                    origin_task_id
                                        .as_deref()
                                        .and_then(|origin| chat_project(state, origin))
                                })
                                .or(saved_project),
                            true,
                            None,
                            definition.source_task_id.clone(),
                            origin_task_id.clone(),
                        ),
                    }
                }
                (None, AutomationScope::Conversation(chat)) => (
                    chat_project(state, chat),
                    false,
                    Some(chat.clone()),
                    Some(chat.clone()),
                    Some(chat.clone()),
                ),
                (None, AutomationScope::Project(project)) => {
                    let local = local_projects(state);
                    let project = project
                        .clone()
                        .or_else(|| {
                            state
                                .selected_space
                                .clone()
                                .filter(|id| local.iter().any(|space| &space.id == id))
                        })
                        .or_else(|| local.first().map(|space| space.id.clone()));
                    (project, true, None, None, None)
                }
            }
        };
        let Some(project) = project_id.clone() else {
            self.error = Some(match &self.scope {
                AutomationScope::Conversation(_) => {
                    "This conversation has no project, so it can't own an automation.".into()
                }
                AutomationScope::Project(_) => {
                    "Add a project on this device to create an automation.".into()
                }
            });
            cx.notify();
            return;
        };
        self.error = None;
        let zone = definition
            .as_ref()
            .map(|definition| definition.timezone.clone())
            .or_else(luna_engine::automation::system_timezone)
            .unwrap_or_else(|| "UTC".into());
        // The schedule's own fields; the others keep their defaults so a
        // kind switch starts from sensible values.
        let mut kind = TriggerKind::Once;
        let mut weekdays = [false; 7];
        let (mut date, mut time) = (String::new(), String::new());
        let (mut every, mut rule) = ("60".to_string(), String::new());
        let (mut repo, mut number, mut poll, mut budget) = (
            String::new(),
            String::new(),
            "15".to_string(),
            "10".to_string(),
        );
        let mut anchor_ms = now_ms();
        let split = |ms: i64| {
            let shown = luna_engine::automation::format_in_zone(ms, &zone);
            (
                shown.get(..10).unwrap_or_default().to_string(),
                shown.get(11..16).unwrap_or_default().to_string(),
            )
        };
        match definition.as_ref().map(|definition| &definition.trigger) {
            Some(AutomationTrigger::Once { at_ms }) => {
                (date, time) = split(*at_ms);
            }
            Some(AutomationTrigger::Interval {
                anchor_ms: anchor,
                every_minutes,
            }) => {
                kind = TriggerKind::Interval;
                every = every_minutes.to_string();
                anchor_ms = *anchor;
            }
            Some(AutomationTrigger::Calendar {
                hour,
                minute,
                weekdays: days,
            }) => {
                kind = TriggerKind::Calendar;
                time = format!("{hour:02}:{minute:02}");
                for day in days {
                    if (1..=7).contains(day) {
                        weekdays[(*day - 1) as usize] = true;
                    }
                }
            }
            Some(AutomationTrigger::RRule {
                rule: saved,
                anchor_ms: anchor,
            }) => {
                kind = TriggerKind::Custom;
                rule = saved.clone();
                anchor_ms = *anchor;
                (date, time) = split(*anchor);
            }
            Some(AutomationTrigger::PrWatch {
                repository,
                number: pr,
                poll_minutes,
                wake_budget,
            }) => {
                kind = TriggerKind::PrWatch;
                repo = repository.clone();
                number = pr.to_string();
                poll = poll_minutes.to_string();
                budget = wake_budget.to_string();
            }
            None => {}
        }
        // Execution: a saved choice is shown as picks; a legacy record shows
        // everything it captured as picks too (account and model options
        // included, so changing one field keeps the others), while an
        // untouched editor still saves it as captured (`None`); a new
        // automation from a chat starts from that chat's config, from the
        // page with the project's defaults.
        let saved_execution = definition
            .as_ref()
            .and_then(|definition| definition.execution.clone());
        let initial_draft = match (&existing, &saved_execution) {
            (Some(_), Some(execution)) => draft_of(execution),
            (Some(record), None) => draft_of(&AutomationExecution::from(&record.authority.config)),
            (None, _) => source_task_id
                .as_deref()
                .and_then(|chat| {
                    self.state
                        .read(cx)
                        .chats
                        .iter()
                        .find(|row| row.id == chat)
                        .and_then(|row| row.config.as_ref())
                        .map(|config| draft_of(&AutomationExecution::from(config)))
                })
                .unwrap_or_default(),
        };
        let (pickers, pickers_changed) =
            Self::make_pickers(&self.state, &project, initial_draft.clone(), cx);
        let editor = Editor {
            automation_id: existing
                .as_ref()
                .map(|record| record.id.clone())
                .unwrap_or_else(|| format!("auto-{}", uuid::Uuid::new_v4())),
            request_id: format!("req-{}", uuid::Uuid::new_v4()),
            expected_revision: existing.as_ref().map_or(0, |record| record.revision),
            existing: existing.is_some(),
            paused: existing
                .as_ref()
                .is_some_and(|record| Lifecycle::of(record) == Lifecycle::Paused),
            kind,
            weekdays,
            name: Self::input(
                "Name",
                definition.as_ref().map_or("", |d| d.name.as_str()),
                cx,
            ),
            prompt: Self::input(
                "What each run sends",
                definition.as_ref().map_or("", |d| d.prompt.as_str()),
                cx,
            ),
            timezone: Self::input("IANA time zone, e.g. Europe/Bucharest", &zone, cx),
            date: Self::input("YYYY-MM-DD", &date, cx),
            time: Self::input("HH:MM", &time, cx),
            every: Self::input("Minutes", &every, cx),
            lateness: Self::input(
                "Minutes",
                &definition
                    .as_ref()
                    .map_or(10, |d| d.max_lateness_ms / 60_000)
                    .to_string(),
                cx,
            ),
            rule: Self::input("FREQ=WEEKLY;BYDAY=MO,WE;BYHOUR=9;BYMINUTE=30", &rule, cx),
            repository: Self::input("owner/repository", &repo, cx),
            number: Self::input("PR number", &number, cx),
            poll: Self::input("Minutes", &poll, cx),
            budget: Self::input("Wakes", &budget, cx),
            project_id,
            fresh,
            chat_id,
            source_task_id,
            origin_task_id,
            notification_policy: definition
                .as_ref()
                .map_or(AutomationNotificationPolicy::ImportantUpdates, |d| {
                    d.notification_policy
                }),
            archive_successful_runs: definition
                .as_ref()
                .is_some_and(|d| d.archive_successful_runs),
            pickers,
            initial_draft,
            saved_execution,
            initial_target: definition.as_ref().map(|d| d.target.clone()),
            _pickers_changed: pickers_changed,
            anchor_ms,
            preview: None,
            phase: Phase::Editing,
            more: false,
            error_basis: None,
        };
        self.editor = Some(editor);
        self.action = None;
        cx.notify();
    }

    /// Move the destination to another project of this host. A chosen chat
    /// from the previous project is cleared (never sent to silently), and
    /// the pickers follow the new project's host and defaults, keeping the
    /// explicit picks.
    fn set_project(&mut self, project: String, cx: &mut Context<Self>) {
        let keep_chat = self
            .editor
            .as_ref()
            .and_then(|editor| editor.chat_id.as_deref())
            .is_some_and(|chat| {
                chat_project(self.state.read(cx), chat).as_deref() == Some(project.as_str())
            });
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        if editor.project_id.as_deref() == Some(project.as_str()) {
            return;
        }
        let draft = editor.pickers.read(cx).draft().clone();
        let (pickers, changed) = Self::make_pickers(&self.state, &project, draft, cx);
        if let Some(editor) = self.editor.as_mut() {
            editor.project_id = Some(project);
            if !keep_chat {
                editor.chat_id = None;
            }
            editor.pickers = pickers;
            editor._pickers_changed = changed;
        }
        cx.notify();
    }

    /// What the Model row says beside the pickers: a provider or account an
    /// existing chat can't run, a reasoning pick the chosen model doesn't
    /// offer (and the level runs use instead), that a legacy automation
    /// still uses what it captured, or that an existing chat fixes the
    /// provider.
    fn model_note(editor: &Editor, state: &AppState, cx: &App) -> Option<String> {
        let pickers = editor.pickers.read(cx);
        let resolved = pickers.resolved(cx);
        let chat = editor.chat_id.as_deref().filter(|_| !editor.fresh);
        if let (Some(chat), Some(harness)) = (chat, resolved.harness)
            && let Some(problem) = chat_provider_mismatch(
                state,
                chat,
                harness,
                resolved.provider_account_id.as_deref(),
            )
        {
            return Some(problem);
        }
        if let (Some(picked), Some(effective)) = (pickers.draft().reasoning, resolved.reasoning)
            && picked != effective
        {
            return Some(format!(
                "{} isn't offered by this model; runs use {}.",
                crate::pickers::reasoning_label(picked),
                crate::pickers::reasoning_label(effective)
            ));
        }
        if editor.existing
            && editor.saved_execution.is_none()
            && !Self::execution_picked(editor, cx)
        {
            return Some("Uses what was captured when it was saved. A pick here pins it.".into());
        }
        let harness = resolved.harness?;
        chat.map(|chat| {
            format!(
                "Runs continue {} on {}: choose its model and effort here. Another provider \
                 needs a new chat each run.",
                chat_name(state, chat),
                crate::shell::board::chat_badges::harness_label(harness)
            )
        })
    }

    /// Whether the user changed what runs execute with since the editor
    /// opened: provider, account, model, reasoning or model options only.
    /// The pickers also carry a checkout, branch and access that follow
    /// new-task defaults on their own; those never count as a pick.
    fn execution_picked(editor: &Editor, cx: &App) -> bool {
        let (now, then) = (editor.pickers.read(cx).draft(), &editor.initial_draft);
        now.harness != then.harness
            || now.provider_account_id != then.provider_account_id
            || now.model != then.model
            || now.reasoning != then.reasoning
            || now.model_options != then.model_options
    }

    /// The execution this save carries: a saved automation keeps exactly
    /// what it has (pinned, or captured for a legacy one) unless its
    /// execution was picked; a new one always pins the resolved provider,
    /// model and reasoning.
    fn editor_execution(editor: &Editor, cx: &App) -> Result<Option<AutomationExecution>, String> {
        if editor.existing && !Self::execution_picked(editor, cx) {
            return Ok(editor.saved_execution.clone());
        }
        let pickers = editor.pickers.read(cx);
        // Until the provider's catalog lands, the resolved reasoning and
        // options are placeholders that change under a review; wait for it.
        if !pickers.catalog_settled(cx) {
            return Err(MODELS_LOADING.into());
        }
        let resolved = pickers.resolved(cx);
        let harness = resolved
            .harness
            .ok_or_else(|| "Choose the provider each run uses.".to_string())?;
        Ok(Some(AutomationExecution {
            harness,
            provider_account_id: resolved.provider_account_id,
            model: resolved.model,
            reasoning: resolved.reasoning,
            model_options: resolved.model_options,
        }))
    }

    /// Where each run goes. An unchanged destination is saved exactly as
    /// it was; a changed one names the chosen chat, or a fresh chat in the
    /// chosen project (keeping an origin only while it is in that project).
    fn editor_target(editor: &Editor, state: &AppState) -> Result<AutomationTarget, String> {
        let project = editor
            .project_id
            .clone()
            .ok_or_else(|| "Choose the project each run belongs to.".to_string())?;
        if !local_projects(state)
            .iter()
            .any(|space| space.id == project)
        {
            return Err("That project is no longer on this device. Choose another project.".into());
        }
        let target = if editor.fresh {
            AutomationTarget::NewConversation {
                origin_task_id: editor.origin_task_id.clone().filter(|origin| {
                    chat_project(state, origin).as_deref() == Some(project.as_str())
                }),
                project_id: Some(project.clone()),
            }
        } else {
            let chat = editor
                .chat_id
                .clone()
                .ok_or_else(|| "Choose the chat each run is sent to.".to_string())?;
            if chat_project(state, &chat).as_deref() != Some(project.as_str()) {
                return Err(format!(
                    "Choose a chat in {}.",
                    project_name(state, &project)
                ));
            }
            AutomationTarget::Existing { task_id: chat }
        };
        // Unchanged destination: the saved target, byte for byte.
        let unchanged = match (&editor.initial_target, &target) {
            (
                Some(AutomationTarget::Existing { task_id: saved }),
                AutomationTarget::Existing { task_id },
            ) => saved == task_id,
            (
                Some(saved @ AutomationTarget::NewConversation { .. }),
                AutomationTarget::NewConversation { .. },
            ) => {
                let saved_project = match saved {
                    AutomationTarget::NewConversation {
                        project_id: Some(project),
                        ..
                    } => Some(project.clone()),
                    AutomationTarget::NewConversation {
                        origin_task_id: Some(origin),
                        ..
                    } => chat_project(state, origin),
                    _ => None,
                };
                saved_project.as_deref() == Some(project.as_str())
            }
            _ => false,
        };
        Ok(match (&editor.initial_target, unchanged) {
            (Some(saved), true) => saved.clone(),
            _ => target,
        })
    }

    /// The editor's schedule alone: its zone and trigger, or the first
    /// field that can't be read (the form's live summary and the saved
    /// definition share it).
    fn editor_schedule(editor: &Editor, cx: &App) -> Result<(String, AutomationTrigger), String> {
        let text = |input: &Entity<ComposerInput>| input.read(cx).text().trim().to_string();
        let number = |input: &Entity<ComposerInput>, what: &str| {
            text(input)
                .parse::<u64>()
                .map_err(|_| format!("Enter a whole number for {what}."))
        };
        let timezone = text(&editor.timezone);
        if timezone.is_empty() {
            return Err("Enter an IANA time zone, e.g. Europe/Bucharest.".into());
        }
        let zoned = |date: &str, time: &str| -> Result<i64, String> {
            let (year, month, day) = parse_date(date)?;
            let (hour, minute) = parse_clock(time)?;
            luna_engine::automation::zoned_ms(&timezone, year, month, day, hour, minute)
        };
        let trigger = match editor.kind {
            TriggerKind::Once => AutomationTrigger::Once {
                at_ms: zoned(&text(&editor.date), &text(&editor.time))?,
            },
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
                let (hour, minute) = parse_clock(&text(&editor.time))?;
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
            TriggerKind::Custom => {
                let rule = text(&editor.rule);
                let body = rule.strip_prefix("RRULE:").unwrap_or(&rule);
                if body.is_empty() {
                    return Err(
                        "Enter a recurrence rule, e.g. FREQ=DAILY;BYHOUR=9;BYMINUTE=0.".into(),
                    );
                }
                if rule.len() > RULE_MAX {
                    return Err(format!("Keep the rule under {RULE_MAX} bytes."));
                }
                if !body
                    .split(';')
                    .any(|part| part.to_ascii_uppercase().starts_with("FREQ="))
                {
                    return Err("A rule needs a frequency, e.g. FREQ=WEEKLY.".into());
                }
                let (date, time) = (text(&editor.date), text(&editor.time));
                let anchor_ms = if date.is_empty() && time.is_empty() {
                    editor.anchor_ms
                } else {
                    zoned(&date, &time)?
                };
                AutomationTrigger::RRule { rule, anchor_ms }
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
        Ok((timezone, trigger))
    }

    /// The editor's fields as a definition (local limits first; the host
    /// validates again).
    fn build_definition(
        editor: &Editor,
        state: &AppState,
        cx: &App,
    ) -> Result<AutomationDefinition, String> {
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
            return Err("Enter what each run sends (up to 32 KiB).".into());
        }
        let target = Self::editor_target(editor, state)?;
        let (timezone, trigger) = Self::editor_schedule(editor, cx)?;
        let lateness = number(&editor.lateness, "the missed-run tolerance")?;
        if lateness > 24 * 60 {
            return Err("The missed-run tolerance is at most 24 hours.".into());
        }
        let execution = Self::editor_execution(editor, cx)?;
        // A run continues an existing chat on that chat's provider and
        // account; the host refuses anything else, so say so before Review.
        if let (AutomationTarget::Existing { task_id }, Some(execution)) = (&target, &execution)
            && let Some(problem) = chat_provider_mismatch(
                state,
                task_id,
                execution.harness,
                execution.provider_account_id.as_deref(),
            )
        {
            return Err(problem);
        }
        Ok(AutomationDefinition {
            name,
            prompt,
            timezone,
            target,
            trigger,
            max_lateness_ms: (lateness * 60_000) as i64,
            execution,
            source_task_id: editor.source_task_id.clone(),
            notification_policy: editor.notification_policy,
            archive_successful_runs: editor.fresh && editor.archive_successful_runs,
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
                definition: Self::build_definition(editor, self.state.read(cx), cx)?,
            },
            preview_id: String::new(),
            permission_ceiling: None,
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
                    editor.error_basis = Some(Err(error.clone()));
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
                    Err(error) => {
                        editor.error_basis = Some(Ok(request));
                        editor.phase = Phase::Error(error);
                    }
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
                AutomationCards::observe_records(std::slice::from_ref(&receipt.record), cx);
                self.apply_receipt(&receipt.record);
                self.editor = None;
                self.refresh(false, cx);
            }
            Mutation::Applied(_) => editor.phase = Phase::Unknown { checking: false },
            Mutation::Refused(reason) => {
                // The refused request is the reviewed one (its preview id
                // aside), which is what the form still builds until edited.
                editor.error_basis = editor
                    .preview
                    .as_ref()
                    .map(|(reviewed, _)| Ok(reviewed.clone()));
                editor.phase = Phase::Error(reason);
            }
            Mutation::Lost => editor.phase = Phase::Unknown { checking: false },
        }
        cx.notify();
    }

    /// The editor's phase as the form shows it: a refusal whose form has
    /// since changed reads as Editing (Review checks the new one). In-flight,
    /// reviewed and unconfirmed phases are never rewritten.
    fn editor_phase(&self, cx: &App) -> Phase {
        let Some(editor) = self.editor.as_ref() else {
            return Phase::Editing;
        };
        match &editor.phase {
            Phase::Error(_)
                if editor
                    .error_basis
                    .as_ref()
                    .is_some_and(|basis| *basis != self.editor_request(cx)) =>
            {
                Phase::Editing
            }
            phase => phase.clone(),
        }
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
                permission_ceiling: None,
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
                AutomationCards::observe_records(std::slice::from_ref(&receipt.record), cx);
                self.apply_receipt(&receipt.record);
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
        chat_name(self.state.read(cx), id)
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

    // ---- menus ----

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_cursor = None;
        if self.menu.begin_close() {
            popover::reap_popup(cx, |panel: &mut Self| &mut panel.menu);
        }
    }

    /// A menu's choices as (value, label, caption), in order.
    fn menu_options(&self, menu: Menu, cx: &App) -> Vec<(String, String, Option<String>)> {
        let state = self.state.read(cx);
        match menu {
            Menu::Project => local_projects(state)
                .into_iter()
                .map(|space| {
                    let caption = Some(space.path.clone());
                    (space.id.clone(), space.display_name().to_string(), caption)
                })
                .collect(),
            Menu::Chat => {
                let Some(project) = self
                    .editor
                    .as_ref()
                    .and_then(|editor| editor.project_id.as_deref())
                else {
                    return Vec::new();
                };
                project_chats(state, project)
                    .into_iter()
                    .map(|chat| {
                        let title = chat_name(state, &chat.id);
                        (chat.id, title, None)
                    })
                    .collect()
            }
            Menu::Notifications => NOTIFICATION_POLICIES
                .iter()
                .map(|policy| {
                    (
                        notification_key(*policy).to_string(),
                        notification_label(*policy).to_string(),
                        Some(notification_caption(*policy).to_string()),
                    )
                })
                .collect(),
        }
    }

    /// The editor's current value for a menu.
    fn menu_value(&self, menu: Menu) -> Option<String> {
        let editor = self.editor.as_ref()?;
        match menu {
            Menu::Project => editor.project_id.clone(),
            Menu::Chat => editor.chat_id.clone(),
            Menu::Notifications => Some(notification_key(editor.notification_policy).into()),
        }
    }

    fn open_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        let options = self.menu_options(menu, cx);
        if options.is_empty() {
            return;
        }
        let value = self.menu_value(menu);
        self.menu_cursor = options
            .iter()
            .position(|(option, _, _)| Some(option) == value.as_ref());
        self.menu.open(menu);
        cx.notify();
    }

    /// Apply a menu choice to the editor.
    fn pick(&mut self, menu: Menu, value: String, cx: &mut Context<Self>) {
        self.close_menu(cx);
        match menu {
            Menu::Project => self.set_project(value, cx),
            Menu::Chat => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.chat_id = Some(value);
                }
            }
            Menu::Notifications => {
                if let Some(editor) = self.editor.as_mut()
                    && let Some(policy) = NOTIFICATION_POLICIES
                        .iter()
                        .find(|policy| notification_key(**policy) == value)
                {
                    editor.notification_policy = *policy;
                }
            }
        }
        cx.notify();
    }

    // ---- focus (an inline card's Open / Edit) ----

    /// Show one automation: highlight its row with its history open, or
    /// (`edit`) open its editor. A record not on the current page is read
    /// by id, so a card always reaches its own automation.
    pub fn focus_automation(&mut self, id: String, edit: bool, cx: &mut Context<Self>) {
        self.focus = Some((id.clone(), edit));
        if self.apply_focus(cx) {
            return;
        }
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let request = AutomationReadRequest {
            automation_id: Some(id.clone()),
            include_deleted: true,
            limit: Some(1),
            ..AutomationReadRequest::default()
        };
        let scope = self.scope.clone();
        self.focus_task = Some(cx.spawn(async move |this, cx| {
            let result = list_page(engine, request).await;
            this.update(cx, |panel, cx| {
                panel.focus_task = None;
                if panel.scope != scope || panel.focus.as_ref().map(|(f, _)| f) != Some(&id) {
                    return;
                }
                match result.map(|page| page.items.into_iter().find(|record| record.id == id)) {
                    Ok(Some(record)) => {
                        AutomationCards::observe_records(std::slice::from_ref(&record), cx);
                        // Not in the loaded rows (another page, or deleted
                        // with Show deleted off): it is listed after them.
                        if !panel.rows().any(|row| row.id == record.id) {
                            panel.pinned = Some(record);
                        }
                        panel.apply_focus(cx);
                    }
                    Ok(None) => {
                        panel.focus = None;
                        panel.error = Some("That automation is not on this device anymore.".into());
                    }
                    Err(error) => {
                        panel.focus = None;
                        panel.error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Act on a pending focus once its row is loaded; `true` when it did.
    fn apply_focus(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((id, edit)) = self.focus.clone() else {
            return false;
        };
        let Some(record) = self.rows().find(|record| record.id == id).cloned() else {
            return false;
        };
        if edit {
            if !record.deleted {
                self.open_editor(Some(record), cx);
            }
        } else if !self.runs.contains_key(&id) {
            self.runs.insert(id.clone(), Runs::default());
            self.load_runs(&id, false, cx);
        }
        self.focus = None;
        self.highlight = Some(id);
        cx.notify();
        true
    }

    /// A definition's status line.
    fn status_line(record: &AutomationRecord) -> String {
        let zone = &record.definition.timezone;
        match Lifecycle::of(record) {
            Lifecycle::Deleted => return "Deleted. Its run history stays.".into(),
            Lifecycle::Finished => {
                return "Finished. It ran and has nothing scheduled; edit its time to run it again."
                    .into();
            }
            Lifecycle::Paused | Lifecycle::Active => {}
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
            AutomationWatch::global(&self.state, cx);
            self.refresh(true, cx);
        }
        let theme = Theme::of(cx).clone();
        // The Automations page is a page; the right pane keeps its surface
        // toolbar. Both share the list, rows and editor below.
        if matches!(self.scope, AutomationScope::Project(_)) {
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
    /// Render as the Automations place's page ([`Self::embedded`]).
    pub fn set_embedded(&mut self, embedded: bool) {
        self.embedded = embedded;
    }

    /// The Automations page: the page frame (title bar with the page's
    /// actions, subtitle naming the project), the editor when one is open,
    /// then the list card, in the page's own scroller with the floating rail.
    fn render_page(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let actions = self.header_actions(ControlSize::Md, theme, cx);
        let editor = self.editor.is_some().then(|| self.render_editor(theme, cx));
        let list_title: SharedString = match self.scope_project() {
            Some(project) => project_name(self.state.read(cx), project).into(),
            None => "All projects on this device".into(),
        };
        let list = self.render_list(list_title, theme, cx);
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
                                "Each automation belongs to a project and runs on this device \
                                 while LUNA is open. The project filter in the sidebar chooses \
                                 which ones are listed. Claude's own scheduled prompts are in the \
                                 Goal popover, not here.",
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
            AutomationScope::Project(_) => "Automations on this device".into(),
        };
        let foreign = self.foreign_host(cx);
        let actions = if foreign.is_some() {
            Vec::new()
        } else {
            self.header_actions(ControlSize::Sm, theme, cx)
        };
        // In the right sidebar's Automations place the place header names
        // it, so the toolbar keeps only the actions.
        let toolbar = crate::surface_chrome::toolbar(theme)
            .child(
                // The chat title is user data: it ellipsizes before the
                // actions are pushed out of the pane.
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(!self.embedded, |el| el.child(SharedString::from(title))),
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
            // Every child keeps its natural height (`flex_none`): this column
            // is height-bounded and scrolls, and a shrinkable editor or list
            // card would be squeezed until its rows overlap.
            .child(
                widgets::row_description(
                    theme,
                    "Runs on this device while LUNA is open. Claude's own scheduled prompts are \
                     listed in the Goal popover, not here.",
                )
                .flex_none(),
            );
        if let Some(host) = foreign {
            body = body.child(
                widgets::section_card(theme).mt(px(0.0)).flex_none().child(
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
                body = body.child(self.render_editor(theme, cx).mt(px(0.0)).flex_none());
            }
            body = body.child(
                self.render_list("This conversation".into(), theme, cx)
                    .mt(px(0.0))
                    .flex_none(),
            );
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
    /// toolbar (`Sm`): New automation, the Show deleted filter and Reload.
    fn header_actions(
        &self,
        size: ControlSize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut actions = Vec::new();
        if self.editor.is_none() {
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
    fn render_list(&self, title: SharedString, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        // Rows read before the panel was hidden wait for the fresh page: a
        // deleted or changed automation is never shown, even briefly.
        let rows: Vec<AutomationRecord> = self.shown_rows().cloned().collect();
        let more = !self.revalidating && self.rows_next().is_some();
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
        if (self.loading && self.first.is_empty()) || self.revalidating {
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
            let creatable = self.editor.is_none();
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_tile(theme, icons::STOPWATCH_PLAY))
                    .child(widgets::row_text(
                        theme,
                        "No automations yet",
                        Some(
                            if self.scope_chat().is_some() {
                                "Send a chat a message on a schedule, start a fresh chat each \
                                 run, or wake this one when a pull request changes."
                            } else {
                                "Send a project's chat a message on a schedule, or start a fresh \
                                 chat in the project each run."
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

    /// Where a definition sends each run, naming the project.
    fn destination_line(&self, definition: &AutomationDefinition, cx: &App) -> String {
        let state = self.state.read(cx);
        match &definition.target {
            AutomationTarget::Existing { task_id } => {
                let project = chat_project(state, task_id)
                    .map(|project| project_name(state, &project))
                    .unwrap_or_else(|| "an unknown project".into());
                format!("Sends to {} in {project}", chat_name(state, task_id))
            }
            AutomationTarget::NewConversation {
                origin_task_id,
                project_id,
            } => {
                let project = project_id
                    .clone()
                    .or_else(|| {
                        origin_task_id
                            .as_deref()
                            .and_then(|origin| chat_project(state, origin))
                    })
                    .map(|project| project_name(state, &project))
                    .unwrap_or_else(|| "its project".into());
                format!("Starts a new chat in {project} each run")
            }
        }
    }

    /// The row's summary, its signature line: when it runs, where each run
    /// goes, and what happens next, in that order.
    fn summary(&self, record: &AutomationRecord, cx: &App) -> Vec<String> {
        let definition = &record.definition;
        let state = self.state.read(cx);
        let destination = match &definition.target {
            AutomationTarget::Existing { task_id } if self.scope_chat() == Some(task_id) => {
                "In this chat".to_string()
            }
            AutomationTarget::Existing { task_id } => format!("In {}", chat_name(state, task_id)),
            AutomationTarget::NewConversation { .. } => "New chat each run".to_string(),
        };
        let now = now_ms();
        let next = match Lifecycle::of(record) {
            Lifecycle::Deleted => "Run history kept".to_string(),
            Lifecycle::Finished => "Ran; nothing scheduled".to_string(),
            Lifecycle::Paused if one_shot_spent(record, now) => {
                "Its time passed; choose a new one".to_string()
            }
            Lifecycle::Paused => "No new runs".to_string(),
            Lifecycle::Active => match record.next_due_ms {
                Some(next) => format!("Next {}", when_brief(next, &definition.timezone, now)),
                None => "Nothing scheduled".to_string(),
            },
        };
        vec![describe_trigger(definition), destination, next]
    }

    /// The row's quiet second line: what runs it and who hears about it.
    fn meta(record: &AutomationRecord) -> Vec<String> {
        let definition = &record.definition;
        let mut meta = vec![
            record_execution_label(record),
            match definition.notification_policy {
                AutomationNotificationPolicy::AllRuns => "Notifies every run",
                AutomationNotificationPolicy::ImportantUpdates => "Notifies on problems",
                AutomationNotificationPolicy::Never => "Notifications off",
            }
            .to_string(),
        ];
        if matches!(definition.target, AutomationTarget::NewConversation { .. })
            && definition.archive_successful_runs
        {
            meta.push("Archives successful runs".into());
        }
        meta
    }

    /// Everything else a row knows, shown with its history: the facts a
    /// glance doesn't need but a check does, as label and value pairs.
    fn details(&self, record: &AutomationRecord, cx: &App) -> Vec<(&'static str, String)> {
        let definition = &record.definition;
        let mut details = vec![
            ("Time zone", definition.timezone.clone()),
            ("Destination", self.destination_line(definition, cx)),
            (
                "Model",
                if definition.execution.is_some() {
                    record_execution_label(record)
                } else {
                    format!("{} (captured when saved)", record_execution_label(record))
                },
            ),
            (
                "Notifications",
                notification_label(definition.notification_policy).to_string(),
            ),
            (
                "Skips runs late by",
                plural(
                    (definition.max_lateness_ms.max(0) as u64).div_ceil(60_000),
                    "minute",
                    "minutes",
                ),
            ),
            (
                "Runs on",
                format!(
                    "{} while LUNA is open",
                    self.host_name(&record.owner_host_id, cx)
                ),
            ),
            (
                "Access",
                format!(
                    "Up to {}",
                    crate::execution_view::permission_title(record.authority.permission_ceiling)
                ),
            ),
        ];
        if let Some(source) = &definition.source_task_id {
            details.push(("Created from", self.chat_title(source, cx)));
        }
        details.push(("Revision", record.revision.to_string()));
        details
    }

    /// One automation as a list row: a status tile, the name and lifecycle,
    /// the summary (cadence · destination · next run) in the primary tone,
    /// a quiet meta line, then the actions (the enable toggle first, Edit,
    /// History, and Delete as an icon that only opens a confirm). Its
    /// confirm flow, details and run history open as full-width blocks
    /// under it. A deleted row keeps History.
    fn render_record(
        &self,
        record: &AutomationRecord,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let definition = &record.definition;
        let lifecycle = Lifecycle::of(record);
        let status_badge = match lifecycle {
            Lifecycle::Active => widgets::badge_active(theme, lifecycle.label()),
            _ => widgets::badge(theme, lifecycle.label()),
        };
        let mut title = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_x(ui_rems(8.0))
            .gap_y(ui_rems(4.0))
            .child(widgets::row_title(theme, definition.name.clone()))
            .child(status_badge);
        // Every project at once: each row names its project, so identical
        // names in two projects are told apart.
        if self.scope_project().is_none() && self.scope_chat().is_none() {
            let project = project_name(self.state.read(cx), &record.authority.project_id);
            title = title.child(widgets::badge(theme, project));
        }
        let fragments = |parts: Vec<String>| {
            parts
                .into_iter()
                .map(|part| SharedString::from(part).into_any_element())
                .collect::<Vec<_>>()
        };
        let summary = self.summary(record, cx);
        let mut text = div()
            .flex_1()
            .min_w(ui_rems(widgets::ROW_TEXT_MIN))
            .flex()
            .flex_col()
            .gap(ui_rems(2.0))
            .child(title)
            .child(
                widgets::meta_line(theme, fragments(summary))
                    .mt(ui_rems(2.0))
                    .text_color(theme.text),
            )
            .child(widgets::meta_line(theme, fragments(Self::meta(record))).mt(px(0.0)));
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
        let highlighted = self.highlight.as_deref() == Some(record.id.as_str());
        let tile = widgets::row_tile(
            theme,
            if lifecycle == Lifecycle::Active {
                icons::STOPWATCH_PLAY
            } else {
                icons::STOPWATCH
            },
        )
        .self_start();
        let mut row = widgets::card_row(theme, false)
            .id(SharedString::from(format!("automation-{}", record.id)))
            .items_start()
            .when(highlighted, |row| {
                row.bg(theme.fill(crate::theme::FillTier::Quaternary))
            })
            .child(tile)
            .child(text);
        let history_open = self.runs.contains_key(&record.id);
        let mut controls = widgets::row_controls();
        if !record.deleted {
            let toggled = record.clone();
            let toggle = RowToggle::for_record(record, now_ms());
            controls = controls.child(
                crate::button::button(
                    format!("automation-toggle-{}", record.id),
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label(format!("{} {}", toggle.label(), definition.name))
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
            );
            // Edit time already opens the editor.
            if toggle != RowToggle::EditTime {
                let edit = record.clone();
                controls = controls.child(
                    crate::button::button(
                        format!("automation-edit-{}", record.id),
                        ButtonKind::Secondary,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .aria_label(format!("Edit {}", definition.name))
                    .child("Edit")
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_editor(Some(edit.clone()), cx)),
                    ),
                );
            }
        }
        let id = record.id.clone();
        controls = controls.child(
            crate::button::button(
                format!("automation-history-{}", record.id),
                ButtonKind::Ghost,
                ControlSize::Sm,
                true,
                theme,
            )
            .aria_label(if history_open {
                format!("Hide details and history of {}", definition.name)
            } else {
                format!("Details and history of {}", definition.name)
            })
            .aria_expanded(history_open)
            .child("History")
            .child(crate::button::glyph(
                if history_open {
                    icons::ALT_ARROW_UP
                } else {
                    icons::ALT_ARROW_DOWN
                },
                ControlSize::Sm,
            ))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_runs(id.clone(), cx))),
        );
        if !record.deleted {
            let deleted = record.clone();
            controls = controls.child(
                crate::button::icon_button(
                    format!("automation-delete-{}", record.id),
                    icons::TRASH_BIN_MINIMALISTIC,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label(format!("Delete {}", definition.name))
                .tooltip(popover::text_tooltip("Delete…"))
                .tooltip_show_delay(popover::TOOLTIP_DELAY)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.begin_action(&deleted, AutomationOperation::Delete, cx)
                })),
            );
        }
        row = row.child(controls);
        if let Some(action) = self
            .action
            .as_ref()
            .filter(|action| action.automation_id == record.id)
        {
            row = row.child(widgets::row_block().child(self.render_action(action, theme, cx)));
        }
        if let Some(runs) = self.runs.get(&record.id) {
            row = row
                .child(widgets::row_block().child(self.render_details(record, theme, cx)))
                .child(widgets::row_block().child(self.render_runs(record, runs, theme, cx)));
        }
        row.into_any_element()
    }

    /// A row's details as an aligned label/value list (labels in the muted
    /// tone on a fixed column, values wrapping beside them).
    fn render_details(&self, record: &AutomationRecord, theme: &Theme, cx: &App) -> gpui::Div {
        let mut list = div()
            .id(SharedString::from(format!(
                "automation-details-{}",
                record.id
            )))
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(4.0));
        for (label, value) in self.details(record, cx) {
            list = list.child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .gap(ui_rems(12.0))
                    .text_size(ui_rems(widgets::ROW_DESCRIPTION_SIZE))
                    .line_height(ui_rems(16.0))
                    .child(
                        div()
                            .flex_none()
                            .w(ui_rems(116.0))
                            .text_color(theme.readable_muted(0.8))
                            .child(label),
                    )
                    .child(div().flex_1().min_w_0().text_color(theme.text).child(value)),
            );
        }
        div().w_full().child(list)
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
            AutomationOperation::ResolveUnknown { .. } => format!(
                "Resolve this uncertain run of \"{}\". It is acknowledged, not resent: it may or \
                 may not have reached the chat. Later scheduled runs can then go ahead.",
                action.name
            ),
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
            let resolve =
                (run.state == AutomationRunState::Unknown && !record.deleted).then(|| {
                    let record = record.clone();
                    let occurrence = run.id.clone();
                    crate::button::button(
                        format!("automation-run-resolve-{}", run.id),
                        ButtonKind::Secondary,
                        ControlSize::Sm,
                        true,
                        theme,
                    )
                    .aria_label("Resolve this uncertain run without resending it")
                    .child("Resolve…")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_action(
                            &record,
                            AutomationOperation::ResolveUnknown {
                                occurrence_id: occurrence.clone(),
                            },
                            cx,
                        )
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
            if open.is_some() || cancel.is_some() || resolve.is_some() {
                row = row.child(
                    div()
                        .mt(crate::typography::ui_rems(6.0))
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(crate::typography::ui_rems(8.0))
                        .children(open)
                        .children(resolve)
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

    /// A pop-up button for one of the editor's menus and, while open, its
    /// menu: pointer and keyboard (arrows, Enter/Space, Escape) alike.
    fn render_select(
        &self,
        menu: Menu,
        id: &'static str,
        label: String,
        accessible: String,
        icon: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let options = self.menu_options(menu, cx);
        let enabled = !options.is_empty();
        // Painted while open or animating out; handlers act on open only.
        let showing = self.menu.get() == Some(&menu);
        let is_open = move |panel: &Self| panel.menu.is_open() && panel.menu.get() == Some(&menu);
        let open = is_open(self);
        let mut trigger = crate::button::select_frame(id, ControlSize::Md, open, enabled, theme)
            .relative()
            .min_w(ui_rems(180.0))
            .max_w(ui_rems(320.0))
            .aria_label(accessible)
            .child(
                crate::button::glyph(icon, ControlSize::Md).text_color(theme.readable_muted(0.85)),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(crate::button::select_chevron(theme));
        if enabled {
            let values: Vec<String> = options.iter().map(|(value, _, _)| value.clone()).collect();
            let key_values = values.clone();
            trigger = trigger
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|panel, _, _, _| panel.menu.note_trigger_press()),
                )
                .on_click(cx.listener(move |panel, event: &gpui::ClickEvent, _, cx| {
                    let open = is_open(panel);
                    if event.is_keyboard() {
                        if open {
                            let pick = panel.menu_cursor.and_then(|ix| values.get(ix)).cloned();
                            match pick {
                                Some(value) => panel.pick(menu, value, cx),
                                None => panel.close_menu(cx),
                            }
                        } else {
                            panel.open_menu(menu, cx);
                        }
                    } else if panel.menu.take_press_was_open() || open {
                        // This press's outside-click already closed it.
                        panel.close_menu(cx);
                    } else {
                        panel.open_menu(menu, cx);
                    }
                    cx.notify();
                }))
                .on_key_down(cx.listener(move |panel, event: &KeyDownEvent, _, cx| {
                    let modifiers = event.keystroke.modifiers;
                    let open = is_open(panel);
                    let delta = match popover::classify_key(
                        &event.keystroke.key,
                        modifiers.platform,
                        modifiers.control,
                    ) {
                        popover::MenuKey::Up => -1,
                        popover::MenuKey::Down => 1,
                        popover::MenuKey::Escape if open => {
                            panel.close_menu(cx);
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        _ => return,
                    };
                    if open {
                        panel.menu_cursor =
                            popover::menu_step(panel.menu_cursor, key_values.len(), delta);
                    } else {
                        panel.open_menu(menu, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }));
        }
        if showing {
            let closing = self.menu.closing_since();
            let value = self.menu_value(menu);
            let mut rows = Vec::with_capacity(options.len());
            for (ix, (option, title, caption)) in options.into_iter().enumerate() {
                let picked = option.clone();
                rows.push(
                    popover::menu_row_nav(
                        theme,
                        value.as_deref() == Some(option.as_str()),
                        self.menu_cursor == Some(ix),
                    )
                    .id(SharedString::from(format!("{id}-option-{ix}")))
                    .role(gpui::Role::MenuItemRadio)
                    .aria_label(title.clone())
                    .on_mouse_move(cx.listener(move |panel, _: &gpui::MouseMoveEvent, _, cx| {
                        if panel.menu_cursor != Some(ix) {
                            panel.menu_cursor = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        cx.stop_propagation();
                        panel.pick(menu, picked.clone(), cx);
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .children(caption.map(|caption| {
                        div()
                            .flex_none()
                            .max_w(ui_rems(200.0))
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.readable_muted(0.8))
                            .child(caption)
                    }))
                    .into_any_element(),
                );
            }
            let heading = match menu {
                Menu::Project => "Projects on this device",
                Menu::Chat => "Chats in this project",
                Menu::Notifications => "Notifications",
            };
            let card = popover::popover_card(theme)
                .w(ui_rems(320.0))
                .on_mouse_down_out(cx.listener(|panel, _, _, cx| {
                    panel.close_menu(cx);
                    cx.notify();
                }))
                .flex()
                .flex_col()
                .child(popover::menu_heading(theme, heading))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-list")))
                        .max_h(ui_rems(280.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(popover::MENU_GAP))
                        .children(rows),
                )
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below_end(
                format!("{id}-menu"),
                card,
                closing,
            ));
        }
        trigger.into_any_element()
    }

    /// The create/edit form, one layout for both: a header (title, what
    /// saving does, Close); the name and message; the schedule with its
    /// live summary in words; where each run goes (stated once); the model;
    /// "More options" (notifications, archiving, lateness), collapsed to a
    /// summary that names every value; then the host's review and actions.
    fn render_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(editor) = self.editor.as_ref() else {
            return div().id("automation-editor");
        };
        let warning = theme.readable_tone(theme.warning, 1.0);
        let danger = theme.readable_tone(theme.danger_muted, 1.0);
        // Everything the form says about projects and chats, read once (the
        // menus and switches below need the context mutably).
        let (subtitle, project_label, chat_label, no_chats, destination) = {
            let state = self.state.read(cx);
            let subtitle = match (editor.existing, editor.paused) {
                (true, true) => {
                    "Paused. Saving keeps it paused; resume it from its row.".to_string()
                }
                (true, false) => "Saving applies to its next runs.".to_string(),
                (false, _) => match &editor.source_task_id {
                    Some(source) => format!("Created from {}.", chat_name(state, source)),
                    None => "Created from the Automations page.".to_string(),
                },
            };
            let project_label = editor
                .project_id
                .as_deref()
                .map(|project| project_name(state, project))
                .unwrap_or_else(|| "Choose a project".into());
            let chat_label = editor
                .chat_id
                .as_deref()
                .map(|chat| chat_name(state, chat))
                .unwrap_or_else(|| "Choose a chat".into());
            let no_chats = editor
                .project_id
                .as_deref()
                .is_none_or(|project| project_chats(state, project).is_empty());
            let destination = match (editor.fresh, editor.chat_id.as_deref()) {
                (true, _) => format!("Each run starts a new chat in {project_label}."),
                (false, Some(chat)) => format!(
                    "Each run is sent to {} in {project_label}.",
                    chat_name(state, chat)
                ),
                (false, None) => "Choose the chat each run is sent to.".into(),
            };
            (subtitle, project_label, chat_label, no_chats, destination)
        };
        let caption = |label: SharedString| {
            div()
                .text_size(ui_rems(11.0))
                .line_height(ui_rems(14.0))
                .text_color(theme.readable_muted(0.85))
                .child(label)
        };
        // A labelled input: a caption over the field; fields share a line
        // and wrap below their minimum width.
        let field = |label: &'static str, input: &Entity<ComposerInput>, min: f32| {
            div()
                .flex_1()
                .min_w(ui_rems(min))
                .flex()
                .flex_col()
                .gap(ui_rems(4.0))
                .child(caption(label.into()))
                .child(input_frame(input, theme))
        };
        let choice = |id: String, label: &'static str, on: bool, role: gpui::Role| {
            crate::button::chip(id, on, ControlSize::Sm, true, theme)
                .role(role)
                .aria_label(label)
                .child(label)
        };
        // A group: its name over its controls, one hairline above. Every
        // group shares the same edge, rhythm and caption tone.
        let group = |id: &'static str, title: Option<&'static str>| {
            let mut group = widgets::card_row(theme, false)
                .id(id)
                .flex_col()
                .items_start()
                .gap(ui_rems(10.0));
            if let Some(title) = title {
                group = group.child(
                    div()
                        .text_size(ui_rems(12.0))
                        .line_height(ui_rems(16.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.readable_muted(0.9))
                        .child(title),
                );
            }
            group
        };
        // A setting inside a group: its label on a fixed column, the
        // control beside it (both wrap in a narrow pane).
        let setting = |label: &'static str, control: AnyElement| {
            div()
                .w_full()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_x(ui_rems(12.0))
                .gap_y(ui_rems(6.0))
                .child(
                    div()
                        .flex_none()
                        .w(ui_rems(112.0))
                        .text_size(ui_rems(12.0))
                        .line_height(ui_rems(16.0))
                        .text_color(theme.text)
                        .child(label),
                )
                .child(div().min_w_0().child(control))
        };
        let note = |copy: String, color: gpui::Hsla| {
            widgets::row_description(theme, copy)
                .w_full()
                .text_color(color)
        };
        let close = crate::button::button(
            "automation-editor-close",
            ButtonKind::Ghost,
            ControlSize::Sm,
            true,
            theme,
        )
        .aria_label("Close the automation editor")
        .child("Close")
        .on_click(cx.listener(|this, _, _, cx| {
            this.editor = None;
            this.flow_task = None;
            this.close_menu(cx);
            cx.notify();
        }));
        let mut form = widgets::section_card(theme)
            .id("automation-editor")
            // Escape closes an open menu first, then the editor; it never
            // reaches the composer or a stop path behind it.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    if this.menu.is_open() {
                        this.close_menu(cx);
                    } else {
                        this.editor = None;
                        this.flow_task = None;
                    }
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
                Some(subtitle.into()),
                vec![close.into_any_element()],
            ));

        // Name and message.
        form = form.child(
            group("automation-editor-essentials", None)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .child(field("Name", &editor.name, 160.0)),
                )
                .child(div().w_full().flex().child(field(
                    "Message each run sends",
                    &editor.prompt,
                    160.0,
                ))),
        );

        // Schedule: the kind, its fields, and what they mean in words (or
        // the first field that can't be read yet).
        let mut kinds = widgets::row_block().gap(ui_rems(6.0));
        for kind in TriggerKind::ALL {
            kinds = kinds.child(
                choice(
                    format!("automation-kind-{}", kind.key()),
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
        let mut schedule = group("automation-editor-schedule", Some("Schedule")).child(kinds);
        match editor.kind {
            TriggerKind::Once => {
                schedule = schedule.child(
                    widgets::row_block()
                        .items_start()
                        .child(field("Date (YYYY-MM-DD)", &editor.date, 120.0))
                        .child(field("Time (HH:MM)", &editor.time, 96.0))
                        .child(field("Time zone", &editor.timezone, 160.0)),
                );
            }
            TriggerKind::Interval => {
                schedule = schedule.child(
                    widgets::row_block()
                        .items_start()
                        .child(field("Every (minutes)", &editor.every, 96.0))
                        .child(field("Time zone", &editor.timezone, 160.0)),
                );
            }
            TriggerKind::Calendar => {
                let mut days = widgets::row_block().gap(ui_rems(6.0));
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
                            .items_start()
                            .child(field("Time (HH:MM)", &editor.time, 96.0))
                            .child(field("Time zone", &editor.timezone, 160.0)),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(ui_rems(6.0))
                            .child(caption("Days (none chosen: every day)".into()))
                            .child(days),
                    );
            }
            TriggerKind::Custom => {
                let rule = editor.rule.read(cx).text().trim().to_string();
                schedule = schedule
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .child(field("Rule (RRULE)", &editor.rule, 160.0)),
                    )
                    .when(rule.is_empty(), |schedule| {
                        schedule.child(widgets::row_description(
                            theme,
                            "Fields separated by semicolons: FREQ (DAILY, WEEKLY, MONTHLY or \
                             YEARLY; MINUTELY and HOURLY as fixed intervals), INTERVAL, BYDAY \
                             (1MO, -1FR), BYMONTHDAY, BYMONTH, BYHOUR, BYMINUTE, WKST and UNTIL.",
                        ))
                    })
                    .child(
                        widgets::row_block()
                            .items_start()
                            .child(field("Starts on (optional)", &editor.date, 120.0))
                            .child(field("At (optional)", &editor.time, 96.0))
                            .child(field("Time zone", &editor.timezone, 160.0)),
                    );
            }
            TriggerKind::PrWatch => {
                schedule = schedule
                    .child(
                        widgets::row_block()
                            .items_start()
                            .child(field("Repository (owner/repo)", &editor.repository, 160.0))
                            .child(field("Pull request", &editor.number, 96.0)),
                    )
                    .child(
                        widgets::row_block()
                            .items_start()
                            .child(field("Check every (1–1440 min)", &editor.poll, 120.0))
                            .child(field("Wake budget (1–100)", &editor.budget, 120.0)),
                    )
                    .child(widgets::row_description(
                        theme,
                        "Wakes only on meaningful changes. LUNA never posts GitHub comments.",
                    ));
            }
        }
        let schedule_words = match Self::editor_schedule(editor, cx) {
            Ok((zone, trigger)) => {
                let definition = AutomationDefinition {
                    timezone: zone.clone(),
                    trigger,
                    ..AutomationDefinition::default()
                };
                let words = describe_trigger(&definition);
                // Calendar wording already names its zone.
                if words.contains(&zone) || editor.kind == TriggerKind::PrWatch {
                    Ok(words)
                } else {
                    Ok(format!("{words} · {zone}"))
                }
            }
            Err(reason) => Err(reason),
        };
        schedule = schedule.child(match schedule_words {
            Ok(words) => div()
                .id("automation-schedule-summary")
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(ui_rems(6.0))
                .child(
                    crate::icons::icon(icons::CLOCK_CIRCLE)
                        .size(ui_rems(14.0))
                        .flex_none()
                        .text_color(theme.accent),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(ui_rems(13.0))
                        .line_height(ui_rems(18.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(words),
                ),
            Err(reason) => div()
                .id("automation-schedule-summary")
                .w_full()
                .child(note(reason, theme.readable_muted(0.85))),
        });
        form = form.child(schedule);

        // Where each run goes: the project, then a new chat or an existing
        // one. The sentence above the controls is the only restatement.
        let fresh = editor.fresh;
        let mut target = group("automation-editor-destination", Some("Where it runs"))
            .child(note(destination, theme.text))
            .child(setting(
                "Project",
                self.render_select(
                    Menu::Project,
                    "automation-project",
                    project_label.clone(),
                    format!("Project: {project_label}"),
                    icons::FOLDER,
                    theme,
                    cx,
                ),
            ))
            .child(setting(
                "Each run",
                crate::button::segmented(
                    "automation-fresh",
                    vec![
                        crate::button::Segment::new("new", "New chat"),
                        crate::button::Segment::new("existing", "Existing chat"),
                    ],
                    usize::from(!fresh),
                    220.0,
                    ControlSize::Sm,
                    theme,
                    cx.listener(|this, ix: &usize, _, cx| {
                        if let Some(editor) = this.editor.as_mut() {
                            editor.fresh = *ix == 0;
                        }
                        cx.notify();
                    }),
                )
                .into_any_element(),
            ));
        if !fresh {
            target = target.child(setting(
                "Chat",
                self.render_select(
                    Menu::Chat,
                    "automation-chat",
                    chat_label.clone(),
                    format!("Chat: {chat_label}"),
                    icons::CHAT_ROUND_LINE,
                    theme,
                    cx,
                ),
            ));
            if no_chats {
                target = target.child(note(
                    "This project has no open chats on this device. Start each run in a new chat \
                     instead."
                        .into(),
                    warning,
                ));
            }
        }
        form = form.child(target);

        // Provider, model and reasoning: the composer's own chips.
        let model_note = Self::model_note(editor, self.state.read(cx), cx);
        form = form.child(
            group("automation-editor-model", Some("Model"))
                .child(
                    div()
                        .id("automation-model-pickers")
                        .w_full()
                        .min_w_0()
                        .child(editor.pickers.clone()),
                )
                .children(model_note.map(|copy| note(copy, warning))),
        );

        // More options: collapsed to a summary that names every value, so
        // nothing saved is hidden; a refusal about them opens it.
        let lateness = editor.lateness.read(cx).text().trim().to_string();
        let built_error = Self::build_definition(editor, self.state.read(cx), cx).err();
        let late_error = built_error
            .as_deref()
            .is_some_and(|error| error.contains("missed-run tolerance"));
        let more = editor.more || late_error;
        let mut summary = vec![
            format!(
                "Notifications: {}",
                notification_label(editor.notification_policy)
            ),
            format!("skips runs over {lateness} min late"),
        ];
        if fresh {
            summary.push(if editor.archive_successful_runs {
                "archives successful runs".into()
            } else {
                "keeps run chats".into()
            });
        }
        let disclosure = div()
            .id("automation-editor-more-toggle")
            .role(gpui::Role::Button)
            .aria_label("More options")
            .aria_expanded(more)
            .tab_index(0)
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(8.0))
            .cursor_pointer()
            .child(
                div()
                    .text_size(ui_rems(12.0))
                    .line_height(ui_rems(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.readable_muted(0.9))
                    .child("More options"),
            )
            .child(
                crate::icons::icon(if more {
                    icons::ALT_ARROW_UP
                } else {
                    icons::ALT_ARROW_DOWN
                })
                .size(ui_rems(12.0))
                .flex_none()
                .text_color(theme.readable_muted(0.85)),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(editor) = this.editor.as_mut() {
                    editor.more = !editor.more;
                }
                cx.notify();
            }));
        let mut options = widgets::card_row(theme, false)
            .id("automation-editor-more")
            .flex_col()
            .items_start()
            .gap(ui_rems(10.0))
            .child(disclosure);
        if more {
            let policy = notification_label(editor.notification_policy).to_string();
            options = options
                .child(setting(
                    "Notifications",
                    self.render_select(
                        Menu::Notifications,
                        "automation-notifications",
                        policy.clone(),
                        format!("Notifications: {policy}"),
                        icons::BELL,
                        theme,
                        cx,
                    ),
                ))
                .child(note(
                    notification_caption(editor.notification_policy).into(),
                    theme.readable_muted(0.85),
                ));
            if fresh {
                options = options.child(setting(
                    "Archive run chats",
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(ui_rems(8.0))
                        .child(widgets::switch_control(
                            "automation-archive",
                            "Archive successful runs",
                            editor.archive_successful_runs,
                            theme,
                            cx,
                            |this: &mut Self, cx| {
                                if let Some(editor) = this.editor.as_mut() {
                                    editor.archive_successful_runs =
                                        !editor.archive_successful_runs;
                                }
                                cx.notify();
                            },
                        ))
                        .child(
                            widgets::row_description(theme, "When a run finishes without problems")
                                .flex_shrink(1.0),
                        )
                        .into_any_element(),
                ));
            }
            options = options.child(setting(
                "Skip if late by",
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(
                        div()
                            .w(ui_rems(72.0))
                            .child(input_frame(&editor.lateness, theme)),
                    )
                    .child(widgets::row_description(theme, "minutes (0–1440)"))
                    .into_any_element(),
            ));
        } else {
            options = options.child(note(summary.join(" · "), theme.readable_muted(0.85)));
        }
        form = form.child(options);

        // Review: the host's answer for the exact request, then the actions.
        let mut review = div()
            .flex_1()
            .min_w(ui_rems(widgets::ROW_TEXT_MIN))
            .flex()
            .flex_col()
            .gap(ui_rems(2.0));
        let current = self.editor_preview_current(cx);
        // Review waits for the provider's catalog (the request would change
        // under it as the model's reasoning resolves).
        let models_loading = matches!(
            Self::editor_execution(editor, cx),
            Err(reason) if reason == MODELS_LOADING
        );
        let phase = self.editor_phase(cx);
        match &phase {
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
                        "This review expired. Review it again before saving.",
                    )
                    .text_color(warning),
                );
            }
            Phase::Reviewed if current => {
                if let Some((request, preview)) = &editor.preview {
                    let zone = editor.timezone.read(cx).text().trim().to_string();
                    review = review.child(
                        widgets::row_description(theme, preview.description.clone())
                            .text_color(theme.text),
                    );
                    // A paused automation stays paused when saved: the
                    // host's times apply only once it is resumed.
                    if preview.next_occurrences_ms.is_empty() {
                        review = review.child(widgets::row_description(
                            theme,
                            if editor.paused {
                                "Paused: no runs until you resume it."
                            } else {
                                "No runs are scheduled."
                            },
                        ));
                    } else {
                        let now = now_ms();
                        let times: Vec<String> = preview
                            .next_occurrences_ms
                            .iter()
                            .map(|at| when_brief(*at, &zone, now))
                            .collect();
                        review = review.child(widgets::row_description(
                            theme,
                            format!(
                                "{}: {}",
                                if editor.paused {
                                    "After Resume"
                                } else {
                                    "Next runs"
                                },
                                times.join(", ")
                            ),
                        ));
                    }
                    let runs_with = match &request.operation {
                        AutomationOperation::Save { definition } => match &definition.execution {
                            Some(execution) => execution_label(
                                execution.harness,
                                execution.model.as_deref(),
                                execution.reasoning,
                            ),
                            None => "the provider and model it captured".into(),
                        },
                        _ => String::new(),
                    };
                    review = review.child(widgets::row_description(
                        theme,
                        format!(
                            "Runs with {runs_with} on {}, access up to {}.",
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
            Phase::Editing if models_loading => {
                review = review.child(widgets::row_description(theme, MODELS_LOADING));
            }
            Phase::Editing => {
                review = review.child(widgets::row_description(
                    theme,
                    "Review asks the host to check the change and show the next runs before you \
                     save.",
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
        if matches!(phase, Phase::Error(_)) {
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
        let review_enabled = !busy && !models_loading;
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
                    "Save",
                    ButtonKind::Primary,
                    apply_enabled,
                )
                .when(apply_enabled, |apply| {
                    apply.on_click(cx.listener(|this, _, _, cx| this.apply_editor(cx)))
                }),
            );
        }
        form.child(
            widgets::card_row(theme, false)
                .bg(theme.fill(crate::theme::FillTier::Quaternary))
                .child(review)
                .child(buttons),
        )
    }
}

/// Headless QA drives the panel through its own paths.
#[cfg(feature = "appshots-fixture")]
impl AutomationsPanel {
    /// Follow another conversation (`None`: every project).
    pub fn fixture_scope(&mut self, chat: Option<&str>, cx: &mut Context<Self>) {
        let scope = match chat {
            Some(chat) => AutomationScope::Conversation(chat.to_string()),
            None => AutomationScope::Project(None),
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
    /// rule, repository, number, poll, budget; or kind
    /// (once/interval/calendar/custom/pr), target (existing/new), day (1-7
    /// toggles); the menus project, chat and notifications
    /// (all/important/never) through their pick path; archive (true/false);
    /// and the pickers' harness (claude-code, codex, …) and reasoning (low,
    /// medium, high, …) through their existing QA picks.
    pub fn fixture_set(&mut self, field: &str, value: &str, cx: &mut Context<Self>) {
        match field {
            "project" => return self.pick(Menu::Project, value.to_string(), cx),
            "chat" => return self.pick(Menu::Chat, value.to_string(), cx),
            "notifications" => return self.pick(Menu::Notifications, value.to_string(), cx),
            "harness" | "reasoning" => {
                let Some(pickers) = self.editor.as_ref().map(|editor| editor.pickers.clone())
                else {
                    return;
                };
                let value = value.to_string();
                let field = field.to_string();
                pickers.update(cx, |pickers, cx| match field.as_str() {
                    "harness" => {
                        if let Ok(harness) = serde_json::from_value(serde_json::json!(value)) {
                            pickers.fixture_pick_harness(harness, cx);
                        }
                    }
                    _ => {
                        if let Ok(level) = serde_json::from_value(serde_json::json!(value)) {
                            pickers.fixture_pick_reasoning(level, cx);
                        }
                    }
                });
                cx.notify();
                return;
            }
            _ => {}
        }
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
            "rule" => Some(editor.rule.clone()),
            "kind" => {
                editor.kind = TriggerKind::ALL
                    .into_iter()
                    .find(|kind| kind.key() == value)
                    .unwrap_or(TriggerKind::Once);
                None
            }
            "target" => {
                editor.fresh = value == "new";
                None
            }
            "archive" => {
                editor.archive_successful_runs = value == "true";
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
    /// Pick a model as the keyboard does: open the model menu, jump to its
    /// row's slot, close the menu. `false` while the catalog hasn't listed it.
    pub fn fixture_pick_model(
        &mut self,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pickers) = self.editor.as_ref().map(|editor| editor.pickers.clone()) else {
            return false;
        };
        pickers.update(cx, |pickers, cx| {
            pickers.open_model_menu(window, cx);
            let Some(slot) = pickers.model_row_ids(cx).iter().position(|id| id == model) else {
                pickers.escape(cx);
                return false;
            };
            let picked = pickers.jump_model_slot(slot, cx);
            pickers.escape(cx);
            picked
        })
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
            other => match other.strip_prefix("resolve:") {
                Some(occurrence) => AutomationOperation::ResolveUnknown {
                    occurrence_id: occurrence.to_string(),
                },
                None => AutomationOperation::CancelOccurrence {
                    occurrence_id: other.trim_start_matches("cancel:").to_string(),
                },
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
        let state = self.state.read(cx);
        // The scroller as last painted: in the right pane its children are
        // the note, the editor (when open) and the list.
        let scroll = {
            let handle = &self.scroll.scroll;
            serde_json::json!({
                "viewportHeight": f32::from(handle.bounds().size.height),
                "maxOffsetY": f32::from(handle.max_offset().y),
                "paneEditorHeight": matches!(self.scope, AutomationScope::Conversation(_))
                    .then_some(())
                    .and(self.editor.as_ref())
                    .and_then(|_| handle.bounds_for_item(1))
                    .map(|bounds| f32::from(bounds.size.height)),
            })
        };
        serde_json::json!({
            "scope": match &self.scope {
                AutomationScope::Conversation(chat) => serde_json::json!({"conversation": chat}),
                AutomationScope::Project(None) => serde_json::json!("all"),
                AutomationScope::Project(Some(project)) => serde_json::json!({"project": project}),
            },
            "highlight": self.highlight,
            "menu": self.menu.get().map(|menu| format!("{menu:?}")),
            "scroll": scroll,
            "foreignHost": self.foreign_host(cx),
            "loading": self.loading,
            "revalidating": self.revalidating,
            "watchSeq": cx
                .try_global::<WatchGlobal>()
                .and_then(|watch| watch.0.read(cx).seq()),
            "error": self.error,
            "rows": self.shown_rows().map(|record| serde_json::json!({
                "id": record.id,
                "name": record.definition.name,
                "revision": record.revision,
                "enabled": record.enabled,
                "deleted": record.deleted,
                "completed": record.completed,
                "lifecycle": Lifecycle::of(record).label(),
                "status": Self::status_line(record),
                "toggle": (!record.deleted).then(|| RowToggle::for_record(record, now_ms()).label()),
                "trigger": describe_trigger(&record.definition),
                "nextDueMs": record.next_due_ms,
                "target": record.definition.target,
                "projectId": record.authority.project_id,
                "destination": self.destination_line(&record.definition, cx),
                "execution": record.definition.execution,
                "executionLabel": record_execution_label(record),
                "sourceTaskId": record.definition.source_task_id,
                "notificationPolicy": record.definition.notification_policy,
                "archiveSuccessfulRuns": record.definition.archive_successful_runs,
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
            "editor": self.editor.as_ref().map(|editor| {
                let built = Self::build_definition(editor, state, cx);
                let resolved = editor.pickers.read(cx).resolved(cx);
                let resolved = serde_json::json!({
                    "harness": resolved.harness,
                    "model": resolved.model,
                    "reasoning": resolved.reasoning,
                });
                serde_json::json!({
                    "requestId": editor.request_id,
                    "automationId": editor.automation_id,
                    "expectedRevision": editor.expected_revision,
                    "existing": editor.existing,
                    "paused": editor.paused,
                    "kind": editor.kind.key(),
                    "projectId": editor.project_id,
                    "fresh": editor.fresh,
                    "chatId": editor.chat_id,
                    "sourceTaskId": editor.source_task_id,
                    "notificationPolicy": editor.notification_policy,
                    "archiveSuccessfulRuns": editor.archive_successful_runs,
                    "draft": serde_json::to_value(editor.pickers.read(cx).draft()).unwrap_or_default(),
                    "resolved": resolved,
                    "modelNote": Self::model_note(editor, state, cx),
                    "phase": phase(&self.editor_phase(cx)),
                    "previewCurrent": self.editor_preview_current(cx),
                    "nextOccurrencesMs": editor.preview.as_ref().map(|(_, preview)| preview.next_occurrences_ms.clone()),
                    "previewDescription": editor.preview.as_ref().map(|(_, preview)| preview.description.clone()),
                    "rule": describe_rule(editor.rule.read(cx).text().trim()),
                    "built": built.as_ref().err(),
                    "definition": built.ok(),
                })
            }),
            "action": self.action.as_ref().map(|action| serde_json::json!({
                "automationId": action.automation_id,
                "requestId": action.request.request_id,
                "operation": action.request.operation,
                "phase": phase(&action.phase),
            })),
        })
    }
}
