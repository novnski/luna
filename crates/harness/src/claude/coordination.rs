//! Native coordination tasks: Claude dynamic workflows (`local_workflow`),
//! experimental agent-team teammates (`in_process_teammate`) and remote agents
//! (`remote_agent`).
//!
//! These ride the same `system` task frames as native subagents
//! (`local_agent`) and shell jobs (`local_bash`), which keep their existing
//! routing. A task is owned here only once its `task_started` names one of
//! the families above. Later frames are matched by `task_id`, so frames for an
//! unknown id are never guessed onto a task or the root turn.
//!
//! Shapes follow the 2.1.286 binary: the embedded SDK schema for
//! `task_started`, `task_progress`, `task_updated` (`patch.status` in
//! pending/running/completed/failed/killed/paused) and `task_notification`
//! (completed/failed/stopped, `reason: worker_restart`). `workflow_progress` is
//! sent by the workflow emitter but is absent from that schema. It is a full
//! snapshot with logs filtered out (entries `workflow_phase` and
//! `workflow_agent`), and the adapter treats it as optional.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use luna_proto::{
    AgentEvent, NATIVE_TASK_MAX_AGENTS, NATIVE_TASK_MAX_PHASES, NativeTaskEndReason,
    NativeTaskKind, NativeTaskSnapshot, NativeTaskStatus, NativeWorkflowAgent,
    NativeWorkflowAgentState, NativeWorkflowPhase,
};

use super::wire::SystemFrame;

/// Tasks tracked per provider process. Oldest terminal ones are forgotten
/// first; the document ledger keeps its own bounded history.
const MAX_TRACKED_TASKS: usize = 64;
const MAX_LABEL_CHARS: usize = 240;
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_SUMMARY_CHARS: usize = 2_000;
const MAX_ERROR_CHARS: usize = 1_000;
const MAX_PATH_CHARS: usize = 1_024;
const MAX_MODEL_CHARS: usize = 120;

/// Extra `system` frame fields only the task frames carry. Every field is a
/// raw [`Value`] so a wrong-typed field can never fail the whole frame (other
/// consumers share these frames).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct TaskFrameFields {
    #[serde(default)]
    task_type: Value,
    #[serde(default)]
    workflow_name: Value,
    #[serde(default)]
    description: Value,
    #[serde(default)]
    patch: Value,
    #[serde(default)]
    usage: Value,
    #[serde(default)]
    summary: Value,
    #[serde(default)]
    workflow_progress: Value,
    #[serde(default)]
    output_file: Value,
    #[serde(default)]
    reason: Value,
}

fn owned_kind(task_type: &Value) -> Option<NativeTaskKind> {
    match task_type.as_str()? {
        "local_workflow" => Some(NativeTaskKind::Workflow),
        "in_process_teammate" => Some(NativeTaskKind::Teammate),
        "remote_agent" => Some(NativeTaskKind::RemoteAgent),
        _ => None,
    }
}

fn bounded(value: &Value, limit: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    })
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn small_index(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// Tracks native coordination tasks for one provider process.
#[derive(Default)]
pub(crate) struct CoordinationTracker {
    tasks: HashMap<String, NativeTaskSnapshot>,
    order: Vec<String>,
    notified: HashSet<String>,
    revision: u64,
}

impl CoordinationTracker {
    /// `None`: the frame is not a coordination frame this tracker owns, so the
    /// caller keeps its own handling. `Some(events)`: consumed (possibly with
    /// no event when nothing observable changed).
    pub(crate) fn observe(&mut self, frame: &SystemFrame) -> Option<Vec<AgentEvent>> {
        let task_id = frame.task_id.as_deref().filter(|id| !id.is_empty())?;
        let fields = &frame.task_fields;
        let next = match frame.subtype.as_str() {
            "task_started" => {
                let kind = owned_kind(&fields.task_type)?;
                let mut task =
                    self.tasks
                        .get(task_id)
                        .cloned()
                        .unwrap_or_else(|| NativeTaskSnapshot {
                            task_id: task_id.to_owned(),
                            kind,
                            status: NativeTaskStatus::Running,
                            ..Default::default()
                        });
                if task.status.is_terminal() {
                    return Some(Vec::new());
                }
                task.kind = kind;
                if !frame.session_id.is_empty() {
                    task.provider_session_id = Some(frame.session_id.clone());
                }
                if let Some(tool) = frame.tool_use_id.as_deref().filter(|t| !t.is_empty()) {
                    task.tool_use_id = Some(tool.to_owned());
                }
                if let Some(name) = bounded(&fields.workflow_name, MAX_LABEL_CHARS) {
                    task.name = Some(name);
                }
                if let Some(description) = bounded(&fields.description, MAX_DESCRIPTION_CHARS) {
                    task.description = description;
                }
                task
            }
            "task_progress" => {
                let mut task = self.tasks.get(task_id)?.clone();
                if task.status.is_terminal() {
                    return Some(Vec::new());
                }
                // Workflow progress rows describe the most recent agent
                // ("phase: label"); the run's own description stays.
                if let Some(activity) = bounded(&fields.description, MAX_DESCRIPTION_CHARS) {
                    task.last_activity = Some(activity);
                }
                if let Some(summary) = bounded(&fields.summary, MAX_SUMMARY_CHARS) {
                    task.summary = Some(summary);
                }
                apply_usage(&mut task, &fields.usage);
                if let Some(entries) = fields.workflow_progress.as_array() {
                    apply_workflow_progress(&mut task, entries);
                }
                task
            }
            "task_updated" => {
                let mut task = self.tasks.get(task_id)?.clone();
                if task.status.is_terminal() {
                    return Some(Vec::new());
                }
                let patch = &fields.patch;
                match patch.get("status").and_then(Value::as_str) {
                    Some("pending") => task.status = NativeTaskStatus::Pending,
                    Some("running") => task.status = NativeTaskStatus::Running,
                    Some("paused") => task.status = NativeTaskStatus::Paused,
                    Some("completed") => task.status = NativeTaskStatus::Completed,
                    Some("failed") => task.status = NativeTaskStatus::Failed,
                    Some("killed") => task.status = NativeTaskStatus::Stopped,
                    _ => {}
                }
                if let Some(description) = patch
                    .get("description")
                    .and_then(|v| bounded(v, MAX_DESCRIPTION_CHARS))
                {
                    task.description = description;
                }
                if let Some(error) = patch.get("error").and_then(|v| bounded(v, MAX_ERROR_CHARS)) {
                    task.error = Some(error);
                }
                task
            }
            "task_notification" => {
                let mut task = self.tasks.get(task_id)?.clone();
                // One notification per task. The CLI sends `task_updated`'s
                // terminal patch first, so the notification may only enrich
                // (summary, result file, usage) and never flips the status.
                if !self.notified.insert(task_id.to_owned()) {
                    return Some(Vec::new());
                }
                if !task.status.is_terminal() {
                    match frame.status.as_deref() {
                        Some("completed") => task.status = NativeTaskStatus::Completed,
                        Some("failed") => task.status = NativeTaskStatus::Failed,
                        Some("stopped" | "killed") => task.status = NativeTaskStatus::Stopped,
                        // Not a terminal shape this schema defines: observe only.
                        _ => {}
                    }
                    if fields.reason.as_str() == Some("worker_restart") {
                        task.status = NativeTaskStatus::Stopped;
                        task.end_reason = Some(NativeTaskEndReason::WorkerRestart);
                    }
                }
                if let Some(summary) = bounded(&fields.summary, MAX_SUMMARY_CHARS) {
                    task.summary = Some(summary);
                }
                if let Some(path) = bounded(&fields.output_file, MAX_PATH_CHARS) {
                    task.output_file = Some(path);
                }
                if task.tool_use_id.is_none()
                    && let Some(tool) = frame.tool_use_id.as_deref().filter(|t| !t.is_empty())
                {
                    task.tool_use_id = Some(tool.to_owned());
                }
                apply_usage(&mut task, &fields.usage);
                task
            }
            _ => return None,
        };
        Some(self.commit(next).into_iter().collect())
    }

    fn commit(&mut self, mut next: NativeTaskSnapshot) -> Option<AgentEvent> {
        if let Some(previous) = self.tasks.get(&next.task_id)
            && previous.same_observation(&next)
        {
            return None;
        }
        self.revision += 1;
        next.revision = self.revision;
        if !self.tasks.contains_key(&next.task_id) {
            self.order.push(next.task_id.clone());
        }
        self.tasks.insert(next.task_id.clone(), next.clone());
        self.forget_overflow();
        Some(AgentEvent::NativeTaskChanged {
            task: Box::new(next),
        })
    }

    fn forget_overflow(&mut self) {
        while self.order.len() > MAX_TRACKED_TASKS {
            let position = self
                .order
                .iter()
                .position(|id| self.tasks.get(id).is_some_and(|t| t.status.is_terminal()))
                .unwrap_or(0);
            let id = self.order.remove(position);
            self.tasks.remove(&id);
            self.notified.remove(&id);
        }
    }
}

fn apply_usage(task: &mut NativeTaskSnapshot, usage: &Value) {
    if let Some(tokens) = usage.get("total_tokens").and_then(number) {
        task.total_tokens = Some(tokens);
    }
    if let Some(tools) = usage.get("tool_uses").and_then(number) {
        task.tool_uses = Some(tools);
    }
    if let Some(duration) = usage.get("duration_ms").and_then(number) {
        task.duration_ms = Some(duration);
    }
}

/// `workflow_progress` is the run's full current graph (logs excluded), so it
/// replaces the previous phase/agent lists instead of appending to them.
fn apply_workflow_progress(task: &mut NativeTaskSnapshot, entries: &[Value]) {
    let mut phases: Vec<NativeWorkflowPhase> = Vec::new();
    let mut agents: Vec<NativeWorkflowAgent> = Vec::new();
    for entry in entries {
        match entry.get("type").and_then(Value::as_str) {
            Some("workflow_phase") => {
                let (Some(index), Some(title)) = (
                    entry.get("index").and_then(small_index),
                    entry.get("title").and_then(|v| bounded(v, MAX_LABEL_CHARS)),
                ) else {
                    continue;
                };
                match phases.iter().position(|phase| phase.index == index) {
                    Some(at) => phases[at].title = title,
                    None if phases.len() < NATIVE_TASK_MAX_PHASES => {
                        phases.push(NativeWorkflowPhase { index, title })
                    }
                    None => {}
                }
            }
            Some("workflow_agent") => {
                let Some(index) = entry.get("index").and_then(small_index) else {
                    continue;
                };
                let state = match entry.get("state").and_then(Value::as_str) {
                    Some("start") => NativeWorkflowAgentState::Started,
                    Some("progress") => NativeWorkflowAgentState::Running,
                    Some("done") => NativeWorkflowAgentState::Done,
                    Some("error") => NativeWorkflowAgentState::Failed,
                    _ => NativeWorkflowAgentState::Unknown,
                };
                let agent = NativeWorkflowAgent {
                    index,
                    label: entry
                        .get("label")
                        .and_then(|v| bounded(v, MAX_LABEL_CHARS))
                        .unwrap_or_else(|| format!("Agent {index}")),
                    phase_index: entry.get("phaseIndex").and_then(small_index),
                    state,
                    model: entry.get("model").and_then(|v| bounded(v, MAX_MODEL_CHARS)),
                    tokens: entry.get("tokens").and_then(number),
                    tool_calls: entry.get("toolCalls").and_then(number),
                    cached: entry
                        .get("cached")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    blocked: entry
                        .get("blocked")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    error: entry.get("error").and_then(|v| bounded(v, MAX_ERROR_CHARS)),
                };
                match agents.iter().position(|a| a.index == index) {
                    Some(at) => agents[at] = agent,
                    None if agents.len() < NATIVE_TASK_MAX_AGENTS => agents.push(agent),
                    None => {}
                }
            }
            _ => {}
        }
    }
    phases.sort_by_key(|phase| phase.index);
    agents.sort_by_key(|agent| agent.index);
    task.phases = phases;
    task.agents = agents;
    task.graph_observed = true;
}

/// Session-identity variables a parent Claude Code session exports to its own
/// subprocesses. A LUNA-launched Claude child must not inherit them when the
/// engine itself runs under a Claude session: they would join that session's
/// peer-messaging socket and Remote Control bridge, letting foreign sessions
/// inject non-human prompts into a LUNA chat (and the reverse).
pub(crate) const INHERITED_SESSION_ENV: &[&str] = &[
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_JOB_DIR",
];

pub(crate) fn scrub_inherited_session_env(cmd: &mut crate::process::Command) {
    for name in INHERITED_SESSION_ENV {
        cmd.env_remove(name);
    }
}
