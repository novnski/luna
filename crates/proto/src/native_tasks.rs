//! Provider-native coordination work that is neither a subagent spawn, a
//! LUNA child chat: Claude Code dynamic workflows, experimental
//! agent-team teammates and remote agents, plus Claude's external background
//! sessions (`claude agents --json`).
//!
//! Snapshots are observations of the provider's own task registry. They are
//! control-plane state: they never become transcript content, never complete a
//! turn and never carry human authority.

use serde::{Deserialize, Serialize};

/// Documents keep at most this many tasks; terminal ones are evicted first.
pub const NATIVE_TASK_LEDGER_LIMIT: usize = 24;
/// Hard cap even when every task is still active.
pub const NATIVE_TASK_LEDGER_HARD_LIMIT: usize = 64;
pub const NATIVE_TASK_MAX_AGENTS: usize = 256;
pub const NATIVE_TASK_MAX_PHASES: usize = 64;

/// Which provider task family a snapshot belongs to. Native subagents
/// (`local_agent`) keep their transcript/chip routing. Shell observations also
/// retain their original tool-result routing; they never become subagents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NativeTaskKind {
    /// Provider-observed background shell lifecycle, after its tool returns.
    Shell,
    /// Claude `local_workflow`: a script-driven multi-agent run.
    #[default]
    Workflow,
    /// Claude `in_process_teammate`: experimental agent teams (opt-in only).
    Teammate,
    /// Claude `remote_agent`: an agent running on Anthropic's cloud.
    RemoteAgent,
}

impl NativeTaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "Background shell",
            Self::Workflow => "Workflow",
            Self::Teammate => "Teammate (experimental)",
            Self::RemoteAgent => "Remote agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NativeTaskStatus {
    Pending,
    #[default]
    Running,
    /// Native pause (TUI-only control, or a usage-limit wait). Not terminal.
    Paused,
    Completed,
    Failed,
    /// Native `killed`/`stopped`, a worker restart, or the provider process
    /// ending before the task reported a result (see [`NativeTaskEndReason`]).
    Stopped,
}

impl NativeTaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stopped)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }
}

/// Why a task ended without an ordinary completion, failure or user stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeTaskEndReason {
    /// Native `reason: worker_restart`: the resumed process found it orphaned.
    WorkerRestart,
    /// LUNA's provider process ended while the task was still active.
    SessionEnded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowPhase {
    pub index: u32,
    /// As reported; redacted runs report `phase N`. Never invented.
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NativeWorkflowAgentState {
    /// Native `start`: queued or starting.
    #[default]
    Started,
    /// Native `progress`.
    Running,
    /// Native `done` (including a cached replay result).
    Done,
    /// Native `error` (including a safety-classifier block).
    Failed,
    /// A state this LUNA build does not know. Shown as unknown, never running.
    Unknown,
}

impl NativeWorkflowAgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Started => "Queued",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowAgent {
    pub index: u32,
    pub label: String,
    #[serde(default)]
    pub phase_index: Option<u32>,
    #[serde(default)]
    pub state: NativeWorkflowAgentState,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub tool_calls: Option<u64>,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// One provider task as last observed by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NativeTaskSnapshot {
    pub task_id: String,
    #[serde(default)]
    pub kind: NativeTaskKind,
    /// Provider session that owns the task, when the adapter knows it.
    #[serde(default)]
    pub provider_session_id: Option<String>,
    /// The launching tool call (e.g. the `Workflow` tool_use id).
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Workflow `meta.name`, when reported.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: NativeTaskStatus,
    #[serde(default)]
    pub end_reason: Option<NativeTaskEndReason>,
    #[serde(default)]
    pub phases: Vec<NativeWorkflowPhase>,
    #[serde(default)]
    pub agents: Vec<NativeWorkflowAgent>,
    /// The provider sent a per-agent graph at least once. Claude 2.1.286 emits
    /// `workflow_progress` outside its published SDK schema, so its absence is
    /// "not reported", not "no agents".
    #[serde(default)]
    pub graph_observed: bool,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub tool_uses: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub last_activity: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Provider-local result file reported at completion.
    #[serde(default)]
    pub output_file: Option<String>,
    /// Adapter-assigned, monotonic within one provider process.
    #[serde(default)]
    pub revision: u64,
}

impl NativeTaskSnapshot {
    /// Equality that ignores the adapter revision counter.
    pub fn same_observation(&self, other: &Self) -> bool {
        let mut left = self.clone();
        left.revision = other.revision;
        left == *other
    }
}

/// Per-conversation ledger persisted in the session document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NativeTaskLedger {
    #[serde(default)]
    pub tasks: Vec<NativeTaskSnapshot>,
}

impl NativeTaskLedger {
    /// Merge one observation. Terminal state is sticky: a late progress frame
    /// or a second, different terminal never rewrites a finished task's
    /// outcome; a same-outcome observation may still add its result details.
    /// Returns whether the ledger changed.
    pub fn upsert(&mut self, incoming: NativeTaskSnapshot) -> bool {
        if incoming.task_id.is_empty() {
            return false;
        }
        if let Some(existing) = self
            .tasks
            .iter_mut()
            .find(|task| task.task_id == incoming.task_id && task.kind == incoming.kind)
        {
            // A finished task only accepts enrichment of the same terminal
            // state (the provider's notification lands after its status
            // patch); it is never resurrected or flipped to another outcome.
            if existing.same_observation(&incoming)
                || (existing.status.is_terminal() && incoming.status != existing.status)
            {
                return false;
            }
            *existing = incoming;
            return true;
        }
        self.tasks.push(incoming);
        self.evict();
        true
    }

    /// The provider process ended: nothing it owned can still be running.
    /// Returns whether any task changed.
    pub fn settle_session_ended(&mut self) -> bool {
        let mut changed = false;
        for task in &mut self.tasks {
            if !task.status.is_terminal() {
                task.status = NativeTaskStatus::Stopped;
                task.end_reason = Some(NativeTaskEndReason::SessionEnded);
                changed = true;
            }
        }
        changed
    }

    pub fn has_active(&self) -> bool {
        self.tasks.iter().any(|task| !task.status.is_terminal())
    }

    fn evict(&mut self) {
        while self.tasks.len() > NATIVE_TASK_LEDGER_LIMIT {
            match self.tasks.iter().position(|task| task.status.is_terminal()) {
                Some(oldest_terminal) => {
                    self.tasks.remove(oldest_terminal);
                }
                None if self.tasks.len() > NATIVE_TASK_LEDGER_HARD_LIMIT => {
                    self.tasks.remove(0);
                }
                None => break,
            }
        }
    }
}

/// `claude agents --json` row kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeExternalSessionKind {
    Interactive,
    Background,
    #[default]
    Unknown,
}

/// Background-session lifecycle state (`state`, background rows only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeExternalSessionState {
    Working,
    Blocked,
    Done,
    Failed,
    Stopped,
    Unknown,
}

/// Live process activity (`status`, present while the process is alive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeExternalSessionActivity {
    Busy,
    Waiting,
    Idle,
    Unknown,
}

/// One native Claude session the provider's own supervisor reports. LUNA
/// observes these read-only; it never attaches, stops or removes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeExternalSession {
    #[serde(default)]
    pub kind: ClaudeExternalSessionKind,
    /// Short id for `claude attach|logs|stop|rm` (background rows only).
    #[serde(default)]
    pub short_id: Option<String>,
    /// Full native session UUID (`claude --resume`).
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub started_at_ms: Option<u64>,
    #[serde(default)]
    pub state: Option<ClaudeExternalSessionState>,
    #[serde(default)]
    pub activity: Option<ClaudeExternalSessionActivity>,
    #[serde(default)]
    pub waiting_for: Option<String>,
    /// The row reported a process id (the process was alive when listed).
    #[serde(default)]
    pub process_alive: bool,
    /// The process is a direct child of this LUNA engine (a LUNA chat's own
    /// print-mode Claude process), not an external writer.
    #[serde(default)]
    pub owned_by_luna: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeExternalSessionsView {
    #[serde(default)]
    pub checked_at_ms: u64,
    #[serde(default)]
    pub sessions: Vec<ClaudeExternalSession>,
    /// Set when the listing could not be read. An empty `sessions` with no
    /// reason means the CLI reported none.
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub truncated: bool,
}
