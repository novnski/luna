//! Claude native agent/task graph (CC-08).
//!
//! Claude reports native work on two channels:
//! - tagged `assistant` snapshots (`parent_tool_use_id` = the spawn call that
//!   owns the frame) carry a child's text, thinking, tool calls and model;
//! - untagged `system` task frames (`task_started`, `task_progress`,
//!   `task_updated`, `task_notification`) carry lifecycle, usage and terminal
//!   status for agents, shell jobs and workflows, keyed by task id and the
//!   launching tool call (shapes: CLI 2.1.286 SDK schema).
//!
//! This joins them into one ownership graph. A nested agent (a child's own
//! `Agent` call, seen only in that child's snapshot), a child's background job
//! and late or unknown notifications are attributed to the transcript that owns
//! them. Nothing here ever produces an untagged (root) event: an observation
//! with no known owner is dropped.

use std::collections::{HashMap, HashSet, VecDeque};

use luna_proto::{AgentEvent, DoneStatus};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::native_stop::{self, SharedStoppable};
use super::normalize::decode_tool_use;
use super::wire::{MessageBody, SystemFrame};

/// Run-launch opt-in (CLI 2.1.286 `--help`): forward a subagent's text and
/// thinking blocks as tagged messages. Without it a foreground child's
/// thinking display is `omitted` and a background child's nested progress
/// (its own children) is not forwarded. Title runs have no tools and skip it.
pub(crate) const RUN_ARGS: &[&str] = &["--forward-subagent-text"];

/// Tool calls made inside children, kept to resolve which child launched a
/// task. Bounded: a long session makes thousands of calls (oldest evicted).
const MAX_TOOL_OWNERS: usize = 1024;
/// Non-agent tasks (shell jobs, workflows). Settled ones are evicted first.
const MAX_TASKS: usize = 1024;

fn tag(parent: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Subagent {
        parent_tool_use_id: parent.to_owned(),
        event: Box::new(event),
    }
}

fn is_spawn_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task")
}

/// The `task_*` fields of a `system` frame, ALLOWLISTED: nothing else of a
/// system frame (an `init` carries MCP auth/URLs) is retained. Kept as raw
/// JSON so a mistyped field never fails the whole frame.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct TaskFields {
    #[serde(default)]
    description: Option<Value>,
    #[serde(default)]
    summary: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    last_tool_name: Option<Value>,
    #[serde(default)]
    patch: Option<Value>,
    #[serde(default)]
    task_type: Option<Value>,
    #[serde(default)]
    is_backgrounded: Option<Value>,
    #[serde(default)]
    skip_transcript: Option<Value>,
    #[serde(default)]
    ambient: Option<Value>,
    #[serde(default)]
    reason: Option<Value>,
    #[serde(default)]
    output_file: Option<Value>,
}

/// Field lookup shared by [`TaskFields`] and a `task_updated` patch object.
trait Fields {
    fn field(&self, key: &str) -> Option<&Value>;
}

impl Fields for TaskFields {
    fn field(&self, key: &str) -> Option<&Value> {
        match key {
            "description" => self.description.as_ref(),
            "summary" => self.summary.as_ref(),
            "usage" => self.usage.as_ref(),
            "last_tool_name" => self.last_tool_name.as_ref(),
            "patch" => self.patch.as_ref(),
            "task_type" => self.task_type.as_ref(),
            "is_backgrounded" => self.is_backgrounded.as_ref(),
            "skip_transcript" => self.skip_transcript.as_ref(),
            "ambient" => self.ambient.as_ref(),
            "reason" => self.reason.as_ref(),
            "output_file" => self.output_file.as_ref(),
            _ => None,
        }
    }
}

impl Fields for Map<String, Value> {
    fn field(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
}

fn text_field(map: &impl Fields, key: &str) -> Option<String> {
    map.field(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn id_field(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty())
}

/// `{total_tokens, tool_uses, duration_ms}` — the provider's task usage.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TaskUsage {
    total_tokens: Option<u64>,
    tool_uses: Option<u64>,
    duration_ms: Option<u64>,
}

impl TaskUsage {
    fn read(map: &impl Fields) -> Option<Self> {
        let usage = map.field("usage")?.as_object()?;
        let n = |key: &str| usage.get(key).and_then(Value::as_u64);
        let usage = Self {
            total_tokens: n("total_tokens"),
            tool_uses: n("tool_uses"),
            duration_ms: n("duration_ms"),
        };
        (usage != Self::default()).then_some(usage)
    }
}

/// One Agent/Task spawn call, keyed by its tool_use id.
#[derive(Default)]
struct Spawn {
    description: Option<String>,
    /// Older CLIs omit the opening user frame; newer ones echo it once.
    opening_seeded: bool,
    pending_initial_echo: Option<String>,
    /// Last model the child's own snapshots reported.
    model: Option<String>,
    background: Option<bool>,
    /// Background was surfaced once (from the input, start or a patch).
    background_announced: bool,
    /// Ambient/skip_transcript housekeeping: no progress rows.
    hidden: bool,
    /// Material fields of the last emitted progress (flood guard).
    progress_mark: Option<(Option<u64>, Option<String>, Option<String>)>,
    /// `task_updated.patch.error`, preferred over the summary on failure.
    failure: Option<String>,
    settled: bool,
}

#[derive(PartialEq, Eq)]
enum TaskKind {
    Shell,
    Workflow,
    Other,
}

/// A non-agent task (shell job, workflow, monitor…).
struct Task {
    kind: TaskKind,
    tool_use_id: Option<String>,
    /// The child that launched it, when known.
    owner: Option<String>,
    background: bool,
    settled: bool,
}

/// Bounded insertion-ordered map (oldest evicted).
#[derive(Default)]
struct Bounded {
    map: HashMap<String, String>,
    order: VecDeque<String>,
}

impl Bounded {
    fn insert(&mut self, key: &str, value: &str, cap: usize) {
        if self.map.insert(key.to_owned(), value.to_owned()).is_none() {
            self.order.push_back(key.to_owned());
            while self.order.len() > cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }
}

#[derive(Default)]
pub(crate) struct NativeTasks {
    spawns: HashMap<String, Spawn>,
    /// Agent/task id → spawn tool_use id (`SendMessage` addresses agents by
    /// this id; a notification may omit `tool_use_id`).
    agent_spawns: HashMap<String, String>,
    tasks: HashMap<String, Task>,
    task_order: VecDeque<String>,
    /// tool_use id of a call made inside a child → that child's spawn id.
    tool_owner: Bounded,
    /// Calls launched with `run_in_background: true` (bounded like owners).
    background_calls: HashSet<String>,
    background_order: VecDeque<String>,
    /// Live, addressable agent spawns of this run (`stop_task` authority).
    stoppable: SharedStoppable,
}

impl NativeTasks {
    pub fn stoppable(&self) -> SharedStoppable {
        self.stoppable.clone()
    }

    /// Agent id → spawn tool_use id, for `SendMessage` steer re-keying.
    pub fn spawn_for_agent(&self, agent: &str) -> Option<&str> {
        self.agent_spawns.get(agent).map(String::as_str)
    }

    fn note_background_call(&mut self, id: &str, input: &Value) {
        if input.get("run_in_background").and_then(Value::as_bool) == Some(true)
            && self.background_calls.insert(id.to_owned())
        {
            self.background_order.push_back(id.to_owned());
            while self.background_order.len() > MAX_TOOL_OWNERS {
                if let Some(old) = self.background_order.pop_front() {
                    self.background_calls.remove(&old);
                }
            }
        }
    }

    /// Register an Agent/Task call. Which transcript holds its chip is the
    /// engine's to derive from the (tagged or root) call event itself.
    fn register_spawn(&mut self, id: &str, input: &Value) {
        let spawn = self.spawns.entry(id.to_owned()).or_default();
        if spawn.description.is_none() {
            spawn.description = input
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
        }
        if spawn.background.is_none()
            && input.get("run_in_background").and_then(Value::as_bool) == Some(true)
        {
            spawn.background = Some(true);
        }
    }

    /// Seed an opening once, retaining compatibility with seed-only producers.
    pub fn opening_prompt(&mut self, id: &str, input: &Value) -> Option<AgentEvent> {
        self.register_spawn(id, input);
        let prompt = input
            .get("prompt")?
            .as_str()
            .filter(|p| !p.trim().is_empty())?;
        let spawn = self.spawns.get_mut(id)?;
        if spawn.opening_seeded || spawn.settled {
            return None;
        }
        spawn.opening_seeded = true;
        spawn.pending_initial_echo = Some(prompt.to_owned());
        Some(tag(
            id,
            AgentEvent::UserMessage {
                text: prompt.to_owned(),
            },
        ))
    }

    pub fn note_child_content(&mut self, id: &str) {
        if let Some(spawn) = self.spawns.get_mut(id) {
            spawn.pending_initial_echo = None;
        }
    }

    /// Only the first plain, single-text native frame can reconcile the seed.
    /// Mixed/tool-result frames and later identical instructions stay intact.
    pub fn consume_initial_echo(&mut self, id: &str, content: &Value) -> bool {
        let plain = match content {
            Value::String(text) if !text.trim().is_empty() => Some(text.as_str()),
            Value::Array(blocks) if blocks.len() == 1 => blocks[0]
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| *kind == "text")
                .and_then(|_| blocks[0].get("text").and_then(Value::as_str))
                .filter(|text| !text.trim().is_empty()),
            _ => None,
        };
        let Some(spawn) = self.spawns.get_mut(id) else {
            return false;
        };
        if let Some(text) = plain {
            if super::normalize::is_synthetic_user_text(text) {
                if text.trim_start().starts_with("[Request interrupted") {
                    spawn.pending_initial_echo = None;
                }
                return false;
            }
            return spawn.pending_initial_echo.take().as_deref() == Some(text);
        }
        // Any other meaningful user text closes the opening window, including
        // multiple text blocks; it must not suppress a later matching steer.
        if content.as_array().is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    || (block.get("type").and_then(Value::as_str) == Some("text")
                        && block.get("text").and_then(Value::as_str).is_some_and(|t| {
                            !t.trim().is_empty() && !super::normalize::is_synthetic_user_text(t)
                        }))
            })
        }) {
            spawn.pending_initial_echo = None;
        }
        false
    }

    /// Root assistant frame: register its Agent/Task spawn calls (root-owned)
    /// so their completion settles even without a `task_started`.
    pub fn root_calls(&mut self, message: &MessageBody) {
        for block in message.content.as_array().into_iter().flatten() {
            let (Some("tool_use"), Some(id)) = (
                block.get("type").and_then(Value::as_str),
                id_field(block.get("id").and_then(Value::as_str)),
            ) else {
                continue;
            };
            let input = block.get("input").unwrap_or(&Value::Null);
            self.note_background_call(id, input);
            if block
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(is_spawn_tool)
            {
                self.register_spawn(id, input);
            }
        }
    }

    /// A child's tagged assistant snapshot, in block order: its text,
    /// thinking (forwarded with `--forward-subagent-text`) and tool calls,
    /// plus what a nested spawn implies (its opening prompt) and a
    /// `SendMessage` steer re-keyed onto the addressed spawn. The child's
    /// reported model is surfaced once per change.
    pub fn tagged_assistant(&mut self, parent: &str, message: &MessageBody) -> Vec<AgentEvent> {
        if message
            .content
            .as_array()
            .is_some_and(|blocks| !blocks.is_empty())
        {
            self.note_child_content(parent);
        }
        let mut out = Vec::new();
        if let Some(model) = message
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            && let Some(spawn) = self.spawns.get_mut(parent)
            && !spawn.settled
            && spawn.model.as_deref() != Some(model)
        {
            spawn.model = Some(model.to_owned());
            out.push(tag(
                parent,
                AgentEvent::ChildProgress {
                    model: Some(model.to_owned()),
                    total_tokens: None,
                    tool_uses: None,
                    duration_ms: None,
                    last_tool: None,
                    summary: None,
                    background: None,
                },
            ));
        }
        for block in message.content.as_array().into_iter().flatten() {
            let text = |key: &str| {
                block
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| format!("{}\n\n", t.trim_end()))
            };
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = text("text") {
                        out.push(tag(parent, AgentEvent::TextDelta { text }));
                    }
                }
                Some("thinking") => {
                    if let Some(text) = text("thinking") {
                        out.push(tag(parent, AgentEvent::ReasoningDelta { text }));
                    }
                }
                Some("tool_use") => {
                    let Some(id) = id_field(block.get("id").and_then(Value::as_str)) else {
                        continue;
                    };
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let input = block.get("input").unwrap_or(&Value::Null);
                    out.push(tag(
                        parent,
                        AgentEvent::ToolCall {
                            id: id.to_owned(),
                            call: decode_tool_use(name, input),
                        },
                    ));
                    self.tool_owner.insert(id, parent, MAX_TOOL_OWNERS);
                    self.note_background_call(id, input);
                    if is_spawn_tool(name) && id != parent {
                        if let Some(opening) = self.opening_prompt(id, input) {
                            out.push(opening);
                        }
                    }
                    if let Some(steer) = self.send_message_steer(name, input) {
                        out.push(steer);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// A `SendMessage{to: <agent id>}` call never echoes on the addressed
    /// child's feed; surface it there as that child's steer.
    pub fn send_message_steer(&mut self, name: &str, input: &Value) -> Option<AgentEvent> {
        if name != "SendMessage" {
            return None;
        }
        let to = ["to", "recipient"]
            .iter()
            .find_map(|k| input.get(*k))
            .and_then(Value::as_str)?;
        let spawn = self.spawn_for_agent(to)?.to_owned();
        let text = ["message", "content"]
            .iter()
            .find_map(|k| input.get(*k))
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())?;
        self.note_child_content(&spawn);
        Some(tag(
            &spawn,
            AgentEvent::UserMessage {
                text: text.to_owned(),
            },
        ))
    }

    /// `task_*` system frames. `None` when the frame is not a task frame.
    pub fn system(&mut self, frame: &SystemFrame) -> Option<Vec<AgentEvent>> {
        let events = match frame.subtype.as_str() {
            "task_started" => self.started(frame),
            "task_progress" => self.progress(frame),
            "task_updated" => self.updated(frame),
            "task_notification" => self.notification(frame),
            _ => return None,
        };
        Some(events)
    }

    /// The spawn a task frame belongs to: its launching tool call when that
    /// is a known spawn, else its agent id.
    fn spawn_of(&self, frame: &SystemFrame) -> Option<String> {
        if let Some(tool) = id_field(frame.tool_use_id.as_deref())
            && self.spawns.contains_key(tool)
        {
            return Some(tool.to_owned());
        }
        id_field(frame.task_id.as_deref())
            .and_then(|task| self.agent_spawns.get(task))
            .cloned()
    }

    fn started(&mut self, frame: &SystemFrame) -> Vec<AgentEvent> {
        let Some(task_id) = id_field(frame.task_id.as_deref()) else {
            return Vec::new();
        };
        let rest = &frame.native_task_fields;
        let task_type = rest.field("task_type").and_then(Value::as_str);
        let hidden = rest.field("skip_transcript").and_then(Value::as_bool) == Some(true)
            || rest.field("ambient").and_then(Value::as_bool) == Some(true);
        let backgrounded = rest.field("is_backgrounded").and_then(Value::as_bool);
        let tool = id_field(frame.tool_use_id.as_deref());
        // An AGENT task: `subagent_type` (2.1.228+) or `local_agent`.
        // Subagent-owned shell tasks share the subtype without either.
        let is_agent = frame.subagent_type.is_some() || task_type == Some("local_agent");
        if is_agent && let Some(tool) = tool {
            let spawn = self.spawns.entry(tool.to_owned()).or_default();
            if spawn.description.is_none() {
                spawn.description = text_field(rest, "description");
            }
            spawn.hidden |= hidden;
            spawn.background = backgrounded.or(spawn.background);
            if !spawn.settled {
                native_stop::track(&self.stoppable, tool, task_id);
            }
            self.agent_spawns
                .insert(task_id.to_owned(), tool.to_owned());
            return self.announce_background(tool).into_iter().collect();
        }
        let kind = match task_type {
            Some("local_bash") => TaskKind::Shell,
            Some("local_workflow") => TaskKind::Workflow,
            _ => TaskKind::Other,
        };
        let owner = tool.and_then(|t| self.tool_owner.get(t)).map(str::to_owned);
        let background =
            backgrounded == Some(true) || tool.is_some_and(|t| self.background_calls.contains(t));
        if self.tasks.len() >= MAX_TASKS && !self.tasks.contains_key(task_id) {
            // Evict the oldest settled task, else the oldest.
            let victim = self
                .task_order
                .iter()
                .position(|id| self.tasks.get(id).is_some_and(|t| t.settled))
                .unwrap_or(0);
            if let Some(old) = self.task_order.remove(victim) {
                self.tasks.remove(&old);
            }
        }
        if !self.tasks.contains_key(task_id) {
            self.task_order.push_back(task_id.to_owned());
        }
        self.tasks.insert(
            task_id.to_owned(),
            Task {
                kind,
                tool_use_id: tool.map(str::to_owned),
                owner,
                background,
                settled: false,
            },
        );
        Vec::new()
    }

    /// Surface a background child once; later frames repeat the flag.
    fn announce_background(&mut self, spawn_id: &str) -> Option<AgentEvent> {
        let spawn = self.spawns.get_mut(spawn_id)?;
        if spawn.background != Some(true)
            || spawn.background_announced
            || spawn.hidden
            || spawn.settled
        {
            return None;
        }
        spawn.background_announced = true;
        Some(tag(
            spawn_id,
            AgentEvent::ChildProgress {
                model: None,
                total_tokens: None,
                tool_uses: None,
                duration_ms: None,
                last_tool: None,
                summary: None,
                background: Some(true),
            },
        ))
    }

    fn progress(&mut self, frame: &SystemFrame) -> Vec<AgentEvent> {
        let Some(spawn_id) = self.spawn_of(frame) else {
            return Vec::new();
        };
        let rest = &frame.native_task_fields;
        let usage = TaskUsage::read(rest).unwrap_or_default();
        let last_tool = text_field(rest, "last_tool_name");
        let summary = text_field(rest, "summary");
        let Some(spawn) = self.spawns.get_mut(&spawn_id) else {
            return Vec::new();
        };
        if spawn.hidden || spawn.settled {
            return Vec::new();
        }
        // Material change only: tokens and elapsed time move every tick.
        let mark = (usage.tool_uses, last_tool.clone(), summary.clone());
        if spawn.progress_mark.as_ref() == Some(&mark) {
            return Vec::new();
        }
        spawn.progress_mark = Some(mark);
        vec![tag(
            &spawn_id,
            AgentEvent::ChildProgress {
                model: None,
                total_tokens: usage.total_tokens,
                tool_uses: usage.tool_uses,
                duration_ms: usage.duration_ms,
                last_tool,
                summary,
                background: None,
            },
        )]
    }

    fn updated(&mut self, frame: &SystemFrame) -> Vec<AgentEvent> {
        let Some(task_id) = id_field(frame.task_id.as_deref()) else {
            return Vec::new();
        };
        let Some(patch) = frame
            .native_task_fields
            .field("patch")
            .and_then(Value::as_object)
        else {
            return Vec::new();
        };
        if let Some(task) = self.tasks.get_mut(task_id) {
            if patch.get("is_backgrounded").and_then(Value::as_bool) == Some(true) {
                task.background = true;
            }
            return Vec::new();
        }
        let Some(spawn_id) = self.agent_spawns.get(task_id).cloned() else {
            return Vec::new();
        };
        let Some(spawn) = self.spawns.get_mut(&spawn_id) else {
            return Vec::new();
        };
        if let Some(error) = text_field(patch, "error") {
            spawn.failure = Some(error);
        }
        // Terminal status waits for `task_notification`, which carries the
        // summary and final usage; settling here would make the engine drop
        // those as stragglers after the transcript freezes.
        if patch.get("is_backgrounded").and_then(Value::as_bool) == Some(true) {
            // A foreground child moved to the background (Ctrl+B semantics).
            spawn.background = Some(true);
            return self.announce_background(&spawn_id).into_iter().collect();
        }
        Vec::new()
    }

    fn notification(&mut self, frame: &SystemFrame) -> Vec<AgentEvent> {
        let rest = &frame.native_task_fields;
        let raw_status = frame.status.as_deref().unwrap_or("");
        let status = match raw_status {
            "completed" | "complete" | "succeeded" | "success" => DoneStatus::Completed,
            "failed" | "errored" | "error" => DoneStatus::Errored,
            "stopped" | "killed" | "cancelled" | "canceled" | "interrupted" => {
                DoneStatus::Interrupted
            }
            // Non-terminal notification shapes: nothing to close.
            _ => return Vec::new(),
        };
        let summary = text_field(rest, "summary");
        if let Some(spawn_id) = self.spawn_of(frame) {
            let usage = TaskUsage::read(rest);
            let worker_restart = text_field(rest, "reason").as_deref() == Some("worker_restart");
            let Some(spawn) = self.spawns.get_mut(&spawn_id) else {
                return Vec::new();
            };
            if spawn.settled {
                // Late duplicate (a second notification, or one replayed by
                // a resumed worker): the child already settled once.
                return Vec::new();
            }
            spawn.settled = true;
            spawn.pending_initial_echo = None;
            native_stop::untrack(&self.stoppable, &spawn_id);
            // The CLI falls back to the task description as the summary;
            // that is not an outcome.
            let summary = summary.filter(|s| Some(s) != spawn.description.as_ref());
            let mut out = Vec::new();
            if let Some(usage) = usage
                && !spawn.hidden
            {
                out.push(tag(
                    &spawn_id,
                    AgentEvent::ChildProgress {
                        model: None,
                        total_tokens: usage.total_tokens,
                        tool_uses: usage.tool_uses,
                        duration_ms: usage.duration_ms,
                        last_tool: None,
                        summary: None,
                        background: None,
                    },
                ));
            }
            let (result, error) = match status {
                DoneStatus::Completed => (summary, None),
                DoneStatus::Errored => (
                    None,
                    spawn
                        .failure
                        .take()
                        .or(summary)
                        .or_else(|| Some("The agent failed".into())),
                ),
                DoneStatus::Interrupted => (
                    None,
                    worker_restart.then(|| "Stopped: the Claude worker restarted".to_owned()),
                ),
            };
            out.push(tag(
                &spawn_id,
                AgentEvent::Done {
                    status,
                    result,
                    error,
                    session_id: None,
                },
            ));
            return out;
        }
        // A non-agent task. Only a BACKGROUND job launched inside a child is
        // reported, as the final result of its launching call in that
        // child's transcript. Root-owned jobs, workflows and foreground
        // shell tasks (whose own tool_result follows) emit nothing here.
        let task = id_field(frame.task_id.as_deref()).and_then(|id| self.tasks.get_mut(id));
        let (tool, owner, background) = match task {
            Some(task) => {
                if task.settled || task.kind == TaskKind::Workflow {
                    return Vec::new();
                }
                task.settled = true;
                (
                    task.tool_use_id.clone(),
                    task.owner.clone(),
                    task.background,
                )
            }
            None => {
                let tool = id_field(frame.tool_use_id.as_deref()).map(str::to_owned);
                let owner = tool
                    .as_deref()
                    .and_then(|t| self.tool_owner.get(t))
                    .map(str::to_owned);
                let background = tool
                    .as_deref()
                    .is_some_and(|t| self.background_calls.contains(t));
                (tool, owner, background)
            }
        };
        let (Some(tool), Some(owner), true) = (tool, owner, background) else {
            return Vec::new();
        };
        if self.spawns.get(&owner).is_none_or(|spawn| spawn.settled) {
            return Vec::new();
        }
        let word = match status {
            DoneStatus::Completed => "completed",
            DoneStatus::Errored => "failed",
            DoneStatus::Interrupted => "stopped",
        };
        let mut output = match summary {
            Some(summary) => format!("Background job {word}: {summary}"),
            None => format!("Background job {word}"),
        };
        if let Some(file) = text_field(rest, "output_file") {
            output.push_str(&format!("\nOutput file: {file}"));
        }
        vec![tag(
            &owner,
            AgentEvent::ToolResult {
                id: tool,
                is_error: status == DoneStatus::Errored,
                output: Some(output),
                diff: None,
            },
        )]
    }
}
