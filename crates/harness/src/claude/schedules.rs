//! Claude-native scheduled prompts (CC-14): `CronCreate`, `CronDelete`,
//! `CronList` and the dynamic `/loop` wakeup `ScheduleWakeup`.
//!
//! Observation only. Claude owns these schedules and fires them between turns
//! while its process runs; LUNA never creates, fires or recreates them (no
//! duplicate timers). The record keeps Claude's own id, schedule, lifetime and
//! state, from the tool's structured `tool_use_result` first and its result
//! text as a fallback (shapes from the 2.1.286 tool definitions):
//! - `CronCreate {cron, prompt, recurring?=true, durable?}` →
//!   `{id, humanSchedule, recurring, durable?}`;
//!   text `Scheduled recurring job <id> (<human>). … Auto-expires after 7 days.`
//!   or `Scheduled one-shot task <id> (<human>). …`;
//! - `CronDelete {id}` → `Cancelled job <id>.`;
//! - `CronList` → `{jobs:[{id, cron, humanSchedule, prompt, recurring?, durable?}]}`;
//! - `ScheduleWakeup {delaySeconds, reason, prompt, noop}` →
//!   `{scheduledFor, clampedDelaySeconds, wasClamped}`; `{stop:true}` →
//!   `{stopped:true, cancelledWakeups}`.
//!
//! Root calls only: a subagent's schedule calls stay in its own transcript.

use std::collections::HashMap;

use luna_proto::{AgentEvent, NativeSchedule, NativeScheduleKind, NativeScheduleState};
use serde_json::Value;

use super::goals::{content_text, is_root, now_ms};

const RECURRING_LIFETIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_PROMPT: usize = 300;
const WAKEUP_ID: &str = "wakeup";
const TOOLS: [&str; 4] = ["CronCreate", "CronDelete", "CronList", "ScheduleWakeup"];

fn bounded(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) struct ScheduleTracker {
    calls: HashMap<String, (String, Value)>,
    records: Vec<NativeSchedule>,
    session_id: String,
}

impl ScheduleTracker {
    pub(crate) fn new() -> Self {
        Self {
            calls: HashMap::new(),
            records: Vec::new(),
            session_id: String::new(),
        }
    }

    pub(crate) fn observe(&mut self, frame: &Value) -> Vec<AgentEvent> {
        if !is_root(frame) {
            return Vec::new();
        }
        match frame.get("type").and_then(Value::as_str) {
            Some("system") => {
                if frame.get("subtype").and_then(Value::as_str) == Some("init")
                    && let Some(id) = frame.get("session_id").and_then(Value::as_str)
                {
                    self.session_id = id.to_owned();
                }
                Vec::new()
            }
            Some("assistant") => {
                for block in blocks(frame) {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    if block.get("type").and_then(Value::as_str) != Some("tool_use")
                        || !TOOLS.contains(&name)
                        || self.calls.len() >= 128
                    {
                        continue;
                    }
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        self.calls.insert(
                            id.to_owned(),
                            (
                                name.to_owned(),
                                block.get("input").cloned().unwrap_or(Value::Null),
                            ),
                        );
                    }
                }
                Vec::new()
            }
            Some("user") => {
                let results: Vec<&Value> = blocks(frame)
                    .filter(|block| {
                        block.get("type").and_then(Value::as_str) == Some("tool_result")
                    })
                    .collect();
                // The envelope's structured output belongs to the frame's one
                // tool result; with several results, fall back to their text.
                let structured = (results.len() == 1)
                    .then(|| frame.get("tool_use_result"))
                    .flatten()
                    .filter(|value| value.is_object());
                let mut changed = false;
                for block in results {
                    let Some((name, input)) = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.calls.remove(id))
                    else {
                        continue;
                    };
                    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let text = content_text(block.get("content").unwrap_or(&Value::Null));
                    changed |= self.apply(&name, &input, structured, &text);
                }
                if changed {
                    vec![AgentEvent::NativeSchedules {
                        session_id: self.session_id.clone(),
                        schedules: self.records.clone(),
                    }]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn upsert(&mut self, record: NativeSchedule) {
        if let Some(existing) = self.records.iter_mut().find(|r| r.id == record.id) {
            *existing = record;
            return;
        }
        if self.records.len() >= NativeSchedule::LIMIT {
            let now = now_ms();
            let drop = self
                .records
                .iter()
                .position(|r| !r.is_live(now))
                .unwrap_or(0);
            self.records.remove(drop);
        }
        self.records.push(record);
    }

    fn apply(&mut self, name: &str, input: &Value, structured: Option<&Value>, text: &str) -> bool {
        let now = now_ms();
        match name {
            "CronCreate" => {
                let id = structured
                    .and_then(|s| s.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        ["Scheduled recurring job ", "Scheduled one-shot task "]
                            .iter()
                            .find_map(|prefix| text.strip_prefix(prefix))
                            .and_then(|rest| rest.split_whitespace().next())
                            .map(str::to_owned)
                    });
                let Some(id) = id.filter(|id| valid_id(id)) else {
                    return false;
                };
                let recurring = structured
                    .and_then(|s| s.get("recurring"))
                    .and_then(Value::as_bool)
                    .or_else(|| input.get("recurring").and_then(Value::as_bool))
                    .unwrap_or(!text.starts_with("Scheduled one-shot"));
                let durable = structured
                    .and_then(|s| s.get("durable"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| text.contains("Persisted to"));
                let schedule = structured
                    .and_then(|s| s.get("humanSchedule"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        text.split_once(" (")
                            .and_then(|(_, rest)| rest.split_once(')'))
                            .map(|(human, _)| human.to_owned())
                    })
                    .unwrap_or_else(|| {
                        input
                            .get("cron")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    });
                self.upsert(NativeSchedule {
                    id,
                    kind: NativeScheduleKind::Cron,
                    schedule: bounded(&schedule, 120),
                    cron: input
                        .get("cron")
                        .and_then(Value::as_str)
                        .map(|cron| bounded(cron, 120)),
                    prompt: bounded(
                        input.get("prompt").and_then(Value::as_str).unwrap_or(""),
                        MAX_PROMPT,
                    ),
                    recurring,
                    durable,
                    created_at_ms: now,
                    next_fire_at_ms: None,
                    expires_at_ms: recurring.then_some(now + RECURRING_LIFETIME_MS),
                    state: NativeScheduleState::Scheduled,
                });
                true
            }
            "CronDelete" => {
                let id = structured
                    .and_then(|s| s.get("id"))
                    .and_then(Value::as_str)
                    .or_else(|| input.get("id").and_then(Value::as_str));
                let Some(record) = id.and_then(|id| self.records.iter_mut().find(|r| r.id == id))
                else {
                    return false;
                };
                record.state = NativeScheduleState::Cancelled;
                true
            }
            "CronList" => {
                let jobs: Option<Vec<Value>> = structured
                    .and_then(|s| s.get("jobs"))
                    .and_then(Value::as_array)
                    .cloned();
                let listed: Vec<String> = match &jobs {
                    Some(jobs) => jobs
                        .iter()
                        .filter_map(|job| job.get("id").and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect(),
                    None if text.trim() == "No scheduled jobs." => Vec::new(),
                    None => text
                        .lines()
                        .filter_map(|line| line.split_once(" \u{2014} ").map(|(id, _)| id.trim()))
                        .filter(|id| valid_id(id))
                        .map(str::to_owned)
                        .collect(),
                };
                let mut changed = false;
                for record in &mut self.records {
                    if record.kind == NativeScheduleKind::Cron
                        && record.state == NativeScheduleState::Scheduled
                        && !listed.contains(&record.id)
                    {
                        record.state = NativeScheduleState::Ended;
                        changed = true;
                    }
                }
                for job in jobs.unwrap_or_default() {
                    let Some(id) = job
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| valid_id(id))
                    else {
                        continue;
                    };
                    if self.records.iter().any(|r| r.id == id) {
                        continue;
                    }
                    let recurring = job
                        .get("recurring")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.upsert(NativeSchedule {
                        id: id.to_owned(),
                        kind: NativeScheduleKind::Cron,
                        schedule: bounded(
                            job.get("humanSchedule")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                            120,
                        ),
                        cron: job
                            .get("cron")
                            .and_then(Value::as_str)
                            .map(|c| bounded(c, 120)),
                        prompt: bounded(
                            job.get("prompt").and_then(Value::as_str).unwrap_or(""),
                            MAX_PROMPT,
                        ),
                        recurring,
                        // CronList marks only session-only jobs (`durable:false`).
                        durable: job.get("durable").and_then(Value::as_bool).unwrap_or(true),
                        // Created before LUNA saw it: the lifetime is unknown.
                        created_at_ms: now,
                        next_fire_at_ms: None,
                        expires_at_ms: None,
                        state: NativeScheduleState::Scheduled,
                    });
                    changed = true;
                }
                changed
            }
            "ScheduleWakeup" => {
                let stopped = structured
                    .and_then(|s| s.get("stopped"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        input.get("stop").and_then(Value::as_bool) == Some(true)
                            || text.starts_with("Loop stopped")
                    });
                if stopped {
                    let mut changed = false;
                    for record in &mut self.records {
                        if record.kind == NativeScheduleKind::Wakeup
                            && record.state == NativeScheduleState::Scheduled
                        {
                            record.state = NativeScheduleState::Cancelled;
                            changed = true;
                        }
                    }
                    return changed;
                }
                let Some(scheduled_for) = structured
                    .and_then(|s| s.get("scheduledFor"))
                    .and_then(Value::as_i64)
                    .filter(|at| *at > 0)
                else {
                    return false;
                };
                let delay = structured
                    .and_then(|s| s.get("clampedDelaySeconds"))
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| ((scheduled_for - now) / 1000).max(0));
                let reason = input
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(|reason| format!(" — {}", bounded(reason, 160)))
                    .unwrap_or_default();
                self.upsert(NativeSchedule {
                    id: WAKEUP_ID.into(),
                    kind: NativeScheduleKind::Wakeup,
                    schedule: format!("in {}{reason}", human_delay(delay)),
                    cron: None,
                    prompt: bounded(
                        input.get("prompt").and_then(Value::as_str).unwrap_or(""),
                        MAX_PROMPT,
                    ),
                    recurring: false,
                    durable: false,
                    created_at_ms: now,
                    next_fire_at_ms: Some(scheduled_for),
                    expires_at_ms: None,
                    state: NativeScheduleState::Scheduled,
                });
                true
            }
            _ => false,
        }
    }
}

fn human_delay(seconds: i64) -> String {
    if seconds >= 3600 && seconds % 3600 == 0 {
        format!("{}h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{}m", (seconds + 30) / 60)
    } else {
        format!("{seconds}s")
    }
}

fn blocks(frame: &Value) -> impl Iterator<Item = &Value> {
    frame
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| blocks.as_slice())
        .unwrap_or_default()
        .iter()
}
