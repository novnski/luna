//! Claude Code `/goal` (CC-14), mapped onto LUNA's shared goal state.
//!
//! Claude's goal is a completion CONDITION checked after every turn by a small
//! model (a session-scoped prompt Stop hook). It has no token budget and no
//! pause. LUNA drives it only with Claude's own `/goal` command lines; Codex
//! goal methods never reach Claude.
//!
//! What headless stream-json (2.1.286) shows, from the installed binary and a
//! real no-model probe (`evidence/cc14-goals-review/probes`):
//! - a local `/goal …` reply is a synthetic assistant frame carrying
//!   `local_command_run {command:"goal"}` + `local_command_source`, then a
//!   `result` with `local_command:"goal"` and the reply as `result`;
//! - an accepted `/goal <condition>` replies `Goal set: <condition>` and then
//!   runs the goal loop inside ONE stream-json turn (one `result` at the end);
//! - each "not met" verdict is a synthetic user frame
//!   `Stop hook feedback:\n[<condition>]: <reason>`;
//! - an unrecoverable error clears the goal with a `system/informational`
//!   warning `Goal cleared after an unrecoverable error …`;
//! - "met" and "impossible" verdicts never reach stdout. Claude persists
//!   every transition as a `goal_status` attachment in the session transcript
//!   (`<config>/projects/<cwd>/<session>.jsonl`), the same record Claude's own
//!   resume reads. LUNA reads it back (read-only, bounded) when a goal loop
//!   ends, instead of guessing.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use luna_proto::{AgentEvent, GoalEvaluation, GoalStatus, ThreadGoal};
use serde_json::Value;

const MAX_REASON_CHARS: usize = 600;
/// Transcripts can be large; goal rows are found in a bounded tail.
const TRANSCRIPT_TAIL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PROJECT_DIRS: usize = 4_096;

pub(crate) const CLEAR_LINE: &str = "/goal clear";

/// The stdin text that sets a Claude goal: Claude's own command.
pub(crate) fn set_line(condition: &str) -> String {
    format!("/goal {}", condition.trim())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn bounded(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Text of a message `content` (a string, or an array of text blocks).
pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(crate) fn is_root(frame: &Value) -> bool {
    frame.get("parent_tool_use_id").is_none_or(Value::is_null)
}

/// A local `/goal` reply, by Claude's exact wording (binary + probe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalReply {
    Set(String),
    Cleared(String),
    NoGoal,
    Active {
        condition: String,
        checks: u32,
        last_reason: Option<String>,
    },
    /// A gate or limit refused the goal; Claude's own text.
    Rejected(String),
}

pub(crate) fn parse_local_reply(text: &str) -> Option<LocalReply> {
    let text = text.trim();
    if let Some(condition) = text.strip_prefix("Goal set: ") {
        return Some(LocalReply::Set(condition.to_owned()));
    }
    if let Some(condition) = text.strip_prefix("Goal cleared: ") {
        return Some(LocalReply::Cleared(condition.to_owned()));
    }
    if text == "No goal set" || text.starts_with("No goal set. ") {
        return Some(LocalReply::NoGoal);
    }
    if let Some(rest) = text.strip_prefix("Goal active: ") {
        let (head, reason) = match rest.split_once("\nLast check: ") {
            Some((head, reason)) => (head, Some(bounded(reason, MAX_REASON_CHARS))),
            None => (rest, None),
        };
        let head = head.trim_end();
        let (condition, checks) = match head
            .strip_suffix(')')
            .and_then(|inner| inner.rsplit_once(" ("))
        {
            Some((condition, count)) => (
                condition.to_owned(),
                count
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
            ),
            None => (head.to_owned(), 0),
        };
        return Some(LocalReply::Active {
            condition,
            checks,
            last_reason: reason,
        });
    }
    if text.starts_with("/goal is only available")
        || text.starts_with("/goal can't run")
        || text.starts_with("Goal condition is limited to")
    {
        return Some(LocalReply::Rejected(text.to_owned()));
    }
    None
}

/// `<local-command-stdout>TEXT</local-command-stdout>` → TEXT.
fn local_command_text(source: &str) -> Option<String> {
    for tag in ["local-command-stdout", "local-command-stderr"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(rest) = source.trim().strip_prefix(&open) {
            let body = rest.strip_suffix(&close).unwrap_or(rest);
            return Some(
                body.replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&"),
            );
        }
    }
    None
}

#[derive(Debug, Clone)]
struct ActiveGoal {
    condition: String,
    set_at_ms: i64,
    checks: u32,
    last_reason: Option<String>,
}

/// Per-run goal state from the frames LUNA sees, plus the transcript readback
/// at the end of a goal loop.
pub(crate) struct GoalTracker {
    /// The condition LUNA sent with `/goal`, until Claude answers.
    requested: Option<String>,
    active: Option<ActiveGoal>,
    session_id: String,
    /// The latest unrecoverable-error clear text (a clear Claude reported).
    cleared_by_error: bool,
}

/// What a goal loop's end needs from the transcript.
#[derive(Debug, Clone)]
pub(crate) struct ReadbackRequest {
    pub session_id: String,
    pub cwd: String,
    pub result_uuid: Option<String>,
}

impl GoalTracker {
    pub(crate) fn new() -> Self {
        Self {
            requested: None,
            active: None,
            session_id: String::new(),
            cleared_by_error: false,
        }
    }

    pub(crate) fn requested(&mut self, condition: &str) {
        self.requested = Some(condition.trim().to_owned());
        self.cleared_by_error = false;
    }

    pub(crate) fn set_session(&mut self, session_id: &str) {
        if !session_id.is_empty() {
            self.session_id = session_id.to_owned();
        }
    }

    fn goal(&self, status: GoalStatus, active: &ActiveGoal) -> ThreadGoal {
        let now = now_ms();
        ThreadGoal {
            thread_id: self.session_id.clone(),
            objective: active.condition.clone(),
            status,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: ((now - active.set_at_ms) / 1000).max(0),
            created_at: active.set_at_ms / 1000,
            updated_at: now / 1000,
            evaluation: Some(GoalEvaluation {
                checks: active.checks,
                last_reason: active.last_reason.clone(),
            }),
        }
    }

    fn changed(&self, goal: Option<ThreadGoal>, turn_id: Option<String>) -> AgentEvent {
        AgentEvent::GoalChanged {
            thread_id: self.session_id.clone(),
            goal,
            turn_id,
        }
    }

    fn apply_reply(&mut self, reply: LocalReply) -> Vec<AgentEvent> {
        match reply {
            LocalReply::Set(condition) => {
                let condition = self
                    .requested
                    .take()
                    .filter(|requested| requested.trim() == condition.trim())
                    .unwrap_or(condition);
                let keep = self
                    .active
                    .as_ref()
                    .filter(|active| active.condition == condition && active.checks == 0)
                    .is_some();
                if !keep {
                    // A new `/goal` replaces any goal (Claude's semantics).
                    self.active = Some(ActiveGoal {
                        condition,
                        set_at_ms: now_ms(),
                        checks: 0,
                        last_reason: None,
                    });
                }
                let active = self.active.clone().expect("set above");
                vec![self.changed(Some(self.goal(GoalStatus::Active, &active)), None)]
            }
            LocalReply::Cleared(_) | LocalReply::NoGoal => {
                self.requested = None;
                if self.active.take().is_some() {
                    vec![self.changed(None, None)]
                } else {
                    Vec::new()
                }
            }
            LocalReply::Active {
                condition,
                checks,
                last_reason,
            } => {
                let set_at_ms = self
                    .active
                    .as_ref()
                    .filter(|active| active.condition == condition)
                    .map(|active| active.set_at_ms)
                    .unwrap_or_else(now_ms);
                let active = ActiveGoal {
                    condition,
                    set_at_ms,
                    checks,
                    last_reason,
                };
                self.active = Some(active.clone());
                vec![self.changed(Some(self.goal(GoalStatus::Active, &active)), None)]
            }
            LocalReply::Rejected(text) => {
                let requested = self.requested.take();
                let mut out = Vec::new();
                if requested.is_some() || self.active.is_some() {
                    self.active = None;
                    out.push(self.changed(None, None));
                }
                out.push(AgentEvent::Error {
                    message: format!("Claude did not set the goal: {text}"),
                });
                out
            }
        }
    }

    /// One parsed root frame. Returns events now and, at a goal loop's end,
    /// a transcript readback request.
    pub(crate) fn observe(
        &mut self,
        frame: &Value,
        cwd: &str,
    ) -> (Vec<AgentEvent>, Option<ReadbackRequest>) {
        let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
        if !is_root(frame) {
            return (Vec::new(), None);
        }
        match kind {
            "system" => {
                let subtype = frame.get("subtype").and_then(Value::as_str).unwrap_or("");
                if subtype == "init" {
                    if let Some(id) = frame.get("session_id").and_then(Value::as_str) {
                        self.set_session(id);
                    }
                    return (Vec::new(), None);
                }
                let content = frame.get("content").and_then(Value::as_str).unwrap_or("");
                if subtype == "informational"
                    && content.starts_with("Goal cleared after an unrecoverable error")
                {
                    self.requested = None;
                    self.cleared_by_error = true;
                    let mut out = Vec::new();
                    if self.active.take().is_some() {
                        out.push(self.changed(None, None));
                    }
                    out.push(AgentEvent::Error {
                        message: content.to_owned(),
                    });
                    return (out, None);
                }
                (Vec::new(), None)
            }
            "assistant" => {
                let is_goal_command = frame
                    .get("local_command_run")
                    .and_then(|run| run.get("command"))
                    .and_then(Value::as_str)
                    == Some("goal");
                let reply = frame
                    .get("local_command_source")
                    .and_then(Value::as_str)
                    .and_then(local_command_text)
                    .filter(|_| is_goal_command)
                    .and_then(|text| parse_local_reply(&text));
                match reply {
                    // Text-only replies are applied from their `result`.
                    Some(reply @ LocalReply::Set(_)) => (self.apply_reply(reply), None),
                    _ => (Vec::new(), None),
                }
            }
            "user" => {
                if frame.get("isSynthetic").and_then(Value::as_bool) != Some(true) {
                    return (Vec::new(), None);
                }
                let text = content_text(
                    frame
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .unwrap_or(&Value::Null),
                );
                let Some(feedback) = text.strip_prefix("Stop hook feedback:\n") else {
                    return (Vec::new(), None);
                };
                let Some(active) = self.active.as_mut() else {
                    return (Vec::new(), None);
                };
                let exact = format!("[{}]: ", active.condition);
                let reason = feedback.strip_prefix(&exact).or_else(|| {
                    // Claude truncates a long condition inside the brackets.
                    feedback
                        .strip_prefix('[')
                        .and_then(|rest| rest.split_once("]: "))
                        .map(|(_, reason)| reason)
                });
                let Some(reason) = reason else {
                    return (Vec::new(), None);
                };
                active.checks = active.checks.saturating_add(1);
                active.last_reason = Some(bounded(reason, MAX_REASON_CHARS));
                let active = active.clone();
                (
                    vec![self.changed(Some(self.goal(GoalStatus::Active, &active)), None)],
                    None,
                )
            }
            "result" => {
                if let Some(id) = frame.get("session_id").and_then(Value::as_str) {
                    self.set_session(id);
                }
                if frame.get("local_command").and_then(Value::as_str) == Some("goal") {
                    let text = frame.get("result").and_then(Value::as_str).unwrap_or("");
                    return match parse_local_reply(text) {
                        Some(reply) => (self.apply_reply(reply), None),
                        None => (Vec::new(), None),
                    };
                }
                if self.active.is_none() || self.session_id.is_empty() {
                    self.requested = None;
                    return (Vec::new(), None);
                }
                (
                    Vec::new(),
                    Some(ReadbackRequest {
                        session_id: self.session_id.clone(),
                        cwd: cwd.to_owned(),
                        result_uuid: frame.get("uuid").and_then(Value::as_str).map(str::to_owned),
                    }),
                )
            }
            _ => (Vec::new(), None),
        }
    }

    /// Apply the transcript's final goal row at a goal loop's end.
    pub(crate) fn apply_readback(
        &mut self,
        readback: Option<TranscriptGoal>,
        result_uuid: Option<String>,
    ) -> Vec<AgentEvent> {
        let Some(readback) = readback else {
            // No native record: keep the goal as last seen, never guess.
            return match self.active.clone() {
                Some(active) => {
                    vec![self.changed(Some(self.goal(GoalStatus::Active, &active)), None)]
                }
                None => Vec::new(),
            };
        };
        match readback.state {
            TranscriptGoalState::Active => {
                let set_at_ms = self
                    .active
                    .as_ref()
                    .map(|active| active.set_at_ms)
                    .or(readback.set_at_ms)
                    .unwrap_or_else(now_ms);
                let active = ActiveGoal {
                    condition: readback.condition,
                    set_at_ms,
                    checks: readback.checks,
                    last_reason: readback.reason,
                };
                self.active = Some(active.clone());
                vec![self.changed(Some(self.goal(GoalStatus::Active, &active)), None)]
            }
            TranscriptGoalState::Met => {
                let Some(mut active) = self.active.take() else {
                    return Vec::new();
                };
                if let Some(iterations) = readback.iterations {
                    active.checks = iterations;
                }
                active.last_reason = readback.reason.or(active.last_reason);
                let mut goal = self.goal(GoalStatus::Complete, &active);
                if let Some(duration) = readback.duration_ms {
                    goal.time_used_seconds = (duration / 1000).max(0);
                }
                if let Some(tokens) = readback.tokens {
                    goal.tokens_used = tokens.max(0);
                }
                vec![self.changed(
                    Some(goal),
                    Some(
                        result_uuid.unwrap_or_else(|| format!("claude-goal-{}", active.set_at_ms)),
                    ),
                )]
            }
            TranscriptGoalState::Impossible => {
                if self.active.take().is_none() {
                    return Vec::new();
                }
                vec![
                    self.changed(None, None),
                    AgentEvent::Error {
                        message: format!(
                            "Claude's goal check judged the condition impossible and cleared the goal: {}",
                            readback.reason.unwrap_or_else(|| "no reason given".into())
                        ),
                    },
                ]
            }
            TranscriptGoalState::Cleared => {
                if self.active.take().is_some() {
                    vec![self.changed(None, None)]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Claude restores an unfinished goal when a session resumes; seed it
    /// from the record so this run's loop end is read back too. Never when
    /// this run starts its own goal.
    pub(crate) fn wants_seed(&self) -> bool {
        self.active.is_none() && self.requested.is_none()
    }

    pub(crate) fn seed(&mut self, readback: Option<TranscriptGoal>) -> Vec<AgentEvent> {
        if !self.wants_seed() {
            return Vec::new();
        }
        let Some(goal) = readback.filter(|goal| goal.state == TranscriptGoalState::Active) else {
            return Vec::new();
        };
        let active = ActiveGoal {
            condition: goal.condition,
            set_at_ms: goal.set_at_ms.unwrap_or_else(now_ms),
            checks: goal.checks,
            last_reason: goal.reason,
        };
        self.active = Some(active.clone());
        vec![self.changed(Some(self.goal(GoalStatus::Active, &active)), None)]
    }

    pub(crate) fn readback_unresolved(&self, readback: &Option<TranscriptGoal>) -> bool {
        self.active.is_some()
            && !self.cleared_by_error
            && readback
                .as_ref()
                .is_none_or(|goal| goal.state == TranscriptGoalState::Active)
    }
}

// --- transcript readback ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptGoalState {
    Active,
    Met,
    Impossible,
    Cleared,
}

/// The session transcript's latest `goal_status` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptGoal {
    pub condition: String,
    pub state: TranscriptGoalState,
    /// "Not met" checks since the goal was set.
    pub checks: u32,
    pub reason: Option<String>,
    pub iterations: Option<u32>,
    pub duration_ms: Option<i64>,
    pub tokens: Option<i64>,
    /// When the goal was set (the set record's timestamp).
    pub set_at_ms: Option<i64>,
}

impl TranscriptGoal {
    /// The shared goal shape for a readback outside a run.
    pub fn to_thread_goal(&self, session_id: &str) -> Option<ThreadGoal> {
        let status = match self.state {
            TranscriptGoalState::Active => GoalStatus::Active,
            TranscriptGoalState::Met => GoalStatus::Complete,
            TranscriptGoalState::Impossible | TranscriptGoalState::Cleared => return None,
        };
        let now = now_ms();
        let set_at = self.set_at_ms.unwrap_or(now);
        Some(ThreadGoal {
            thread_id: session_id.to_owned(),
            objective: self.condition.clone(),
            status,
            token_budget: None,
            tokens_used: self.tokens.unwrap_or(0).max(0),
            time_used_seconds: self
                .duration_ms
                .map(|ms| ms / 1000)
                .unwrap_or((now - set_at) / 1000)
                .max(0),
            created_at: set_at / 1000,
            updated_at: now / 1000,
            evaluation: Some(GoalEvaluation {
                checks: self.iterations.unwrap_or(self.checks),
                last_reason: self.reason.clone(),
            }),
        })
    }
}

/// Claude's own default config dir, as the child inherits it.
pub fn default_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::executable::home_dir().map(|home| home.join(".claude")))
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Claude's project directory name: the cwd with every non-alphanumeric
/// character replaced by `-` (verified: `/a/b-c` → `-a-b-c`).
fn encoded_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn transcript_path(config_dir: &Path, session_id: &str, cwd: &str) -> Option<PathBuf> {
    if !valid_session_id(session_id) {
        return None;
    }
    let projects = config_dir.join("projects");
    let file = format!("{session_id}.jsonl");
    if !cwd.is_empty() {
        let direct = projects.join(encoded_cwd(cwd)).join(&file);
        if direct.is_file() {
            return Some(direct);
        }
    }
    // Long or unusual paths are encoded differently by newer CLIs; the
    // session id is unique, so search one level of project directories.
    let entries = std::fs::read_dir(&projects).ok()?;
    entries
        .take(MAX_PROJECT_DIRS)
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&file))
        .find(|candidate| candidate.is_file())
}

/// `2026-10-01T14:25:19.175Z` → unix ms (UTC only, as Claude writes them).
fn parse_utc_ms(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next()?.parse().ok()?;
    let millis: i64 = format!("{frac:0<3}")[..3].parse().ok()?;
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

/// Read the session's latest goal record. Read-only and bounded; `None` when
/// the transcript or any goal record is missing.
pub fn read_transcript_goal(
    config_dir: &Path,
    session_id: &str,
    cwd: &str,
) -> Option<TranscriptGoal> {
    read_transcript_goal_as_of(config_dir, session_id, cwd, None)
}

/// The goal record as it stood at `as_of` (unix ms): rows written later are
/// ignored. A resumed run seeds from the goal it inherited, even when its own
/// first verdict lands in the transcript before the seed read does.
pub(crate) fn read_transcript_goal_as_of(
    config_dir: &Path,
    session_id: &str,
    cwd: &str,
    as_of: Option<i64>,
) -> Option<TranscriptGoal> {
    let path = transcript_path(config_dir, session_id, cwd)?;
    let mut file = std::fs::File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    let skip_partial = len > TRANSCRIPT_TAIL_BYTES;
    if skip_partial {
        file.seek(SeekFrom::Start(len - TRANSCRIPT_TAIL_BYTES))
            .ok()?;
    }
    let mut reader = BufReader::new(file.take(TRANSCRIPT_TAIL_BYTES));
    if skip_partial {
        let mut partial = Vec::new();
        reader.read_until(b'\n', &mut partial).ok()?;
    }
    let mut rows: Vec<(Value, Option<i64>)> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !line.contains("\"goal_status\"") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if row.get("type").and_then(Value::as_str) != Some("attachment") {
            continue;
        }
        let Some(attachment) = row.get("attachment") else {
            continue;
        };
        if attachment.get("type").and_then(Value::as_str) != Some("goal_status") {
            continue;
        }
        let at = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_utc_ms);
        if as_of.zip(at).is_some_and(|(cutoff, at)| at > cutoff) {
            continue;
        }
        rows.push((attachment.clone(), at));
    }
    goal_from_rows(&rows)
}

fn goal_from_rows(rows: &[(Value, Option<i64>)]) -> Option<TranscriptGoal> {
    let flag = |row: &Value, key: &str| row.get(key).and_then(Value::as_bool) == Some(true);
    let (last, _) = rows.last()?;
    let condition = last.get("condition").and_then(Value::as_str)?.to_owned();
    let set_index = rows
        .iter()
        .rposition(|(row, _)| flag(row, "sentinel") && !flag(row, "met"));
    let checks = rows
        .iter()
        .skip(set_index.map_or(0, |index| index + 1))
        .filter(|(row, _)| !flag(row, "sentinel") && !flag(row, "met") && !flag(row, "failed"))
        .count() as u32;
    let state = if flag(last, "failed") {
        TranscriptGoalState::Impossible
    } else if flag(last, "met") && flag(last, "sentinel") {
        TranscriptGoalState::Cleared
    } else if flag(last, "met") {
        TranscriptGoalState::Met
    } else {
        TranscriptGoalState::Active
    };
    Some(TranscriptGoal {
        condition,
        state,
        checks,
        reason: last
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| bounded(reason, MAX_REASON_CHARS)),
        iterations: last
            .get("iterations")
            .and_then(Value::as_u64)
            .map(|n| n.min(u32::MAX as u64) as u32),
        duration_ms: last.get("durationMs").and_then(Value::as_i64),
        tokens: last.get("tokens").and_then(Value::as_i64),
        set_at_ms: set_index.and_then(|index| rows[index].1),
    })
}

/// Read with a bounded wait for a verdict Claude may still be writing: only
/// while the last record is unresolved (the verdict rows land within the
/// turn's end, but transcript writes are asynchronous).
pub(crate) async fn read_after_loop(
    tracker: &GoalTracker,
    config_dir: Option<PathBuf>,
    request: &ReadbackRequest,
) -> Option<TranscriptGoal> {
    let config_dir = config_dir?;
    let mut attempt = 0;
    loop {
        let dir = config_dir.clone();
        let session = request.session_id.clone();
        let cwd = request.cwd.clone();
        let readback =
            tokio::task::spawn_blocking(move || read_transcript_goal(&dir, &session, &cwd))
                .await
                .ok()
                .flatten();
        attempt += 1;
        if attempt >= 8 || !tracker.readback_unresolved(&readback) {
            return readback;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}
