//! Native subagents and background jobs (OC-11), on the same contract as
//! Claude and Codex (provider-parity-contract §2):
//! - the v2 `subagent` (v1 `task`) call decodes to `Agent: <description>`;
//!   its chip id is the v2 call id (`session:message:call`).
//! - a child session binds to its spawn chip by the tool's
//!   `metadata.sessionID` (progress, success, failure), else by
//!   `session.created{parentID, title}`, at ANY depth. Every child's traffic
//!   is tagged FLAT with its own chip (`AgentEvent::Subagent`), so a
//!   grandchild's chip lives in its parent child's transcript and the
//!   engine's nested-spawn index links it.
//! - child execution frames become `ChildStatus` (Codex pattern); child model,
//!   tokens and tool calls become `ChildProgress` (Claude pattern).
//! - a chip settles once: foreground on the spawn tool's result, background on
//!   the `session.synthetic` completion, and a stopped child on its own
//!   `execution.interrupted` (never on a stop acknowledgement).
//! - background shell jobs (`shell.*`) become `NativeTaskChanged{Shell}`
//!   rows, like Claude's `local_bash`.
//!
//! Native subagents never become LUNA child chats (PD-6).

use std::collections::HashSet;

use luna_proto::{ChildRuntimeStatus, NativeTaskKind, NativeTaskSnapshot, NativeTaskStatus};

use super::*;

/// Bound on spawns, children and shell jobs remembered per run.
const MAX_TRACKED: usize = 512;
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_ERROR_CHARS: usize = 1_000;
const MAX_PATH_CHARS: usize = 1_024;
const MAX_SYNTHETIC_CHARS: usize = 2_000;
/// The internal payload type carrying the raw v2 observations below.
pub(super) const OBSERVE: &str = "v2.observe";

/// The native spawn tools: v2 `subagent`, v1 `task`.
pub(super) fn is_spawn_tool(name: &str) -> bool {
    matches!(name, "subagent" | "task")
}

fn bounded(text: &str, limit: usize) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    })
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Wrap an event as subagent-attributed traffic.
pub(super) fn tag(parent: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Subagent {
        parent_tool_use_id: parent.to_owned(),
        event: Box::new(event),
    }
}

fn progress(model: Option<String>, child: &ChildRun) -> AgentEvent {
    AgentEvent::ChildProgress {
        model,
        total_tokens: child.total_tokens,
        tool_uses: Some(child.tool_uses),
        duration_ms: None,
        last_tool: child.last_tool.clone(),
        summary: None,
        background: None,
    }
}

fn background_progress() -> AgentEvent {
    AgentEvent::ChildProgress {
        model: None,
        total_tokens: None,
        tool_uses: None,
        duration_ms: None,
        last_tool: None,
        summary: None,
        background: Some(true),
    }
}

/// One child session bound to a spawn chip.
pub(super) struct ChildRun {
    /// The spawn chip (`ToolCall::id`) its traffic is tagged with.
    pub(super) parent_tool_use_id: String,
    pub(super) feed: SessionFeed,
    /// Chip settled (tagged Done sent); late traffic drops.
    pub(super) done: bool,
    /// The session whose spawn created it (the root or another child).
    pub(super) parent_session: String,
    /// Bound by the tool's own metadata (authoritative), not a title guess.
    confirmed: bool,
    /// An execution of this child is running.
    active: bool,
    model: Option<String>,
    tool_uses: u64,
    last_tool: Option<String>,
    total_tokens: Option<u64>,
}

impl ChildRun {
    fn new(chip: &str, parent_session: &str, confirmed: bool) -> Self {
        Self {
            parent_tool_use_id: chip.to_owned(),
            feed: SessionFeed::default(),
            done: false,
            parent_session: parent_session.to_owned(),
            confirmed,
            active: false,
            model: None,
            tool_uses: 0,
            last_tool: None,
            total_tokens: None,
        }
    }
}

/// A spawn call (`subagent`/`task`) seen in a session of this run.
struct Spawn {
    /// The session the spawn tool ran in.
    owner: String,
    description: String,
    prompt: Option<String>,
    background: bool,
    background_announced: bool,
    child: Option<String>,
    settled: bool,
    opening_seeded: bool,
}

/// A native shell job owned by a session of this run.
struct ShellJob {
    snapshot: NativeTaskSnapshot,
    /// Reported as a background row (only background jobs are).
    announced: bool,
}

/// Every native child session and background job of one run, keyed by
/// session id at any depth.
#[derive(Default)]
pub(super) struct ChildTree {
    /// Bound child session → its run.
    pub(super) runs: HashMap<String, ChildRun>,
    /// Child sessions of this tree awaiting their chip (id → title).
    pub(super) unbound: HashMap<String, String>,
    unbound_parent: HashMap<String, String>,
    spawns: HashMap<String, Spawn>,
    spawn_order: VecDeque<String>,
    /// Calls whose input asked for background execution.
    background_calls: HashSet<String>,
    /// Chip → the shell job its tool reported.
    shell_of_call: HashMap<String, String>,
    shells: HashMap<String, ShellJob>,
    shell_order: VecDeque<String>,
    revision: u64,
    /// A root `session.synthetic` landed while no LUNA turn was active: the
    /// provider is about to start a wake execution.
    wake_expected: bool,
}

/// What one observation asks of the run loop.
#[derive(Default)]
pub(super) struct Observed {
    pub(super) events: Vec<AgentEvent>,
    /// The root started an execution LUNA did not prompt (a wake).
    pub(super) wake_started: bool,
}

impl ChildTree {
    /// A session belongs to this run: the root or any known descendant.
    pub(super) fn owns(&self, root: &str, session: &str) -> bool {
        session == root || self.runs.contains_key(session) || self.unbound.contains_key(session)
    }

    /// Background work the provider will report on after the root turn:
    /// an unsettled background spawn or a running background shell.
    pub(super) fn has_background_work(&self) -> bool {
        self.spawns.values().any(|s| s.background && !s.settled)
            || self
                .shells
                .values()
                .any(|job| job.announced && !job.snapshot.status.is_terminal())
    }

    /// Any chip still open (awaiting its own terminal).
    pub(super) fn has_open(&self) -> bool {
        self.spawns
            .values()
            .any(|s| !s.settled && s.child.is_some())
    }

    pub(super) fn take_wake_expected(&mut self) -> bool {
        std::mem::take(&mut self.wake_expected)
    }

    /// The live child a per-child stop may address.
    pub(super) fn stop_target(&self, chip: &str) -> Option<String> {
        let spawn = self.spawns.get(chip).filter(|s| !s.settled)?;
        let child = spawn.child.as_ref()?;
        self.runs
            .get(child)
            .filter(|run| !run.done && run.parent_tool_use_id == chip)
            .map(|_| child.clone())
    }

    /// Live background children, for root Stop.
    pub(super) fn stop_background(&self) -> Vec<String> {
        self.spawns
            .values()
            .filter(|s| s.background && !s.settled)
            .filter_map(|s| s.child.clone())
            .filter(|child| self.runs.get(child).is_some_and(|run| !run.done))
            .collect()
    }

    /// Settle every open chip locally. Only for a run that is dying (the
    /// server crashed, stalled or never answered the root interrupt).
    pub(super) fn settle_all(&mut self, status: DoneStatus) -> Vec<AgentEvent> {
        let open: Vec<String> = self
            .spawns
            .iter()
            .filter(|(_, s)| !s.settled && s.child.is_some())
            .map(|(chip, _)| chip.clone())
            .collect();
        open.into_iter()
            .filter_map(|chip| self.settle(&chip, status, None))
            .collect()
    }

    /// Close one chip with its Done (once).
    fn settle(
        &mut self,
        chip: &str,
        status: DoneStatus,
        error: Option<String>,
    ) -> Option<AgentEvent> {
        let spawn = self.spawns.get_mut(chip)?;
        if spawn.settled {
            return None;
        }
        spawn.settled = true;
        if let Some(child) = spawn.child.as_deref()
            && let Some(run) = self.runs.get_mut(child)
            && run.parent_tool_use_id == chip
        {
            run.done = true;
            run.active = false;
        }
        Some(tag(
            chip,
            AgentEvent::Done {
                status,
                result: None,
                error,
                session_id: None,
            },
        ))
    }

    fn evict_spawns(&mut self) {
        while self.spawn_order.len() > MAX_TRACKED {
            let position = self
                .spawn_order
                .iter()
                .position(|chip| self.spawns.get(chip).is_none_or(|s| s.settled))
                .unwrap_or(0);
            if let Some(chip) = self.spawn_order.remove(position) {
                self.spawns.remove(&chip);
                self.background_calls.remove(&chip);
                self.shell_of_call.remove(&chip);
            }
        }
        while self.runs.len() > MAX_TRACKED {
            let Some(id) = self
                .runs
                .iter()
                .find(|(_, run)| run.done)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.runs.remove(&id);
        }
    }

    /// A spawn call was made in `owner` (any session of this run).
    fn register_spawn(&mut self, owner: &str, chip: &str, input: &Value) -> Vec<AgentEvent> {
        if chip.is_empty() || self.spawns.contains_key(chip) {
            return Vec::new();
        }
        let background = input.get("background").and_then(Value::as_bool) == Some(true);
        self.spawns.insert(
            chip.to_owned(),
            Spawn {
                owner: owner.to_owned(),
                description: str_field(input, "description")
                    .unwrap_or_default()
                    .to_owned(),
                prompt: str_field(input, "prompt").map(str::to_owned),
                background,
                background_announced: false,
                child: None,
                settled: false,
                opening_seeded: false,
            },
        );
        self.spawn_order.push_back(chip.to_owned());
        self.evict_spawns();
        let mut events = self.announce_background(chip);
        // A child that raced ahead of its chip binds now (title, else the
        // only unbound child of that owner).
        let description = self.spawns[chip].description.clone();
        let candidates: Vec<String> = self
            .unbound_parent
            .iter()
            .filter(|(_, parent)| parent.as_str() == owner)
            .map(|(id, _)| id.clone())
            .collect();
        let matched = candidates
            .iter()
            .find(|id| {
                !description.is_empty()
                    && self
                        .unbound
                        .get(*id)
                        .is_some_and(|title| title.starts_with(&description))
            })
            .cloned()
            .or_else(|| (candidates.len() == 1).then(|| candidates[0].clone()));
        if let Some(child) = matched {
            events.extend(self.bind(chip, &child, false));
        }
        events
    }

    /// Surface a background spawn once on its chip.
    fn announce_background(&mut self, chip: &str) -> Vec<AgentEvent> {
        let Some(spawn) = self.spawns.get_mut(chip) else {
            return Vec::new();
        };
        if !spawn.background || spawn.background_announced || spawn.settled {
            return Vec::new();
        }
        spawn.background_announced = true;
        vec![tag(chip, background_progress())]
    }

    /// Bind `child` to `chip`. A confirmed (metadata) binding corrects an
    /// earlier title/FIFO guess and reopens a continued child.
    fn bind(&mut self, chip: &str, child: &str, confirmed: bool) -> Vec<AgentEvent> {
        let Some(owner) = self.spawns.get(chip).map(|s| s.owner.clone()) else {
            return Vec::new();
        };
        // Another child guessed onto this chip goes back to waiting.
        let displaced = self
            .spawns
            .get(chip)
            .and_then(|s| s.child.clone())
            .filter(|bound| bound != child);
        if let Some(displaced) = displaced
            && let Some(run) = self.runs.remove(&displaced)
        {
            self.unbound.insert(displaced.clone(), String::new());
            self.unbound_parent.insert(displaced, run.parent_session);
        }
        match self.runs.get_mut(child) {
            Some(run) if run.parent_tool_use_id == chip => {
                run.confirmed |= confirmed;
            }
            Some(run) => {
                if !confirmed && run.confirmed {
                    return Vec::new();
                }
                // A previous (guessed, or settled and now continued) chip
                // releases this child.
                let previous = std::mem::replace(&mut run.parent_tool_use_id, chip.to_owned());
                run.confirmed |= confirmed;
                run.done = false;
                // The new chip's card starts its own observation (the
                // session's cumulative token total stays OpenCode's own).
                run.active = false;
                run.model = None;
                run.tool_uses = 0;
                run.last_tool = None;
                if let Some(old) = self.spawns.get_mut(&previous)
                    && old.child.as_deref() == Some(child)
                {
                    old.child = None;
                }
            }
            None => {
                self.unbound.remove(child);
                let parent = self.unbound_parent.remove(child).unwrap_or(owner);
                self.runs
                    .insert(child.to_owned(), ChildRun::new(chip, &parent, confirmed));
            }
        }
        if let Some(spawn) = self.spawns.get_mut(chip) {
            spawn.child = Some(child.to_owned());
        }
        self.evict_spawns();
        let mut events = self.announce_background(chip);
        if confirmed {
            events.extend(self.opening(child));
        }
        events
    }

    /// The child's opening prompt, seeded once per chip from the spawn input
    /// (the native prompt carries a "You are a subagent…" preamble).
    pub(super) fn opening(&mut self, child: &str) -> Option<AgentEvent> {
        let chip = self.runs.get(child)?.parent_tool_use_id.clone();
        let spawn = self.spawns.get_mut(&chip)?;
        if spawn.opening_seeded || spawn.settled {
            return None;
        }
        spawn.opening_seeded = true;
        let prompt = spawn.prompt.clone()?;
        Some(tag(&chip, AgentEvent::UserMessage { text: prompt }))
    }

    /// Tag one bound child's transcript events with its chip; the opening
    /// prompt (once) goes first when no confirmed binding seeded it.
    pub(super) fn tagged(&mut self, child: &str, events: Vec<AgentEvent>) -> Vec<AgentEvent> {
        let Some(chip) = self
            .runs
            .get(child)
            .map(|run| run.parent_tool_use_id.clone())
        else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(events.len() + 1);
        if !events.is_empty()
            && let Some(opening) = self.opening(child)
        {
            out.push(opening);
        }
        out.extend(events.into_iter().map(|event| tag(&chip, event)));
        out
    }

    /// `session.created` for a session whose parent is in this run.
    pub(super) fn on_session_created(
        &mut self,
        root: &str,
        child: &str,
        parent: &str,
        title: &str,
    ) -> Vec<AgentEvent> {
        if child.is_empty() || self.runs.contains_key(child) || !self.owns(root, parent) {
            return Vec::new();
        }
        let pending: Vec<String> = self
            .spawn_order
            .iter()
            .filter(|chip| {
                self.spawns
                    .get(*chip)
                    .is_some_and(|s| s.owner == parent && s.child.is_none() && !s.settled)
            })
            .cloned()
            .collect();
        let matched = pending
            .iter()
            .find(|chip| {
                let description = &self.spawns[*chip].description;
                !description.is_empty() && title.starts_with(description.as_str())
            })
            .or_else(|| pending.first())
            .cloned();
        match matched {
            Some(chip) => self.bind(&chip, child, false),
            None => {
                if self.unbound.len() < MAX_TRACKED {
                    self.unbound.insert(child.to_owned(), title.to_owned());
                    self.unbound_parent
                        .insert(child.to_owned(), parent.to_owned());
                }
                Vec::new()
            }
        }
    }

    /// One raw v2 observation (`v2.observe` payload properties).
    pub(super) fn observe(&mut self, root: &str, props: &Value, turn_active: bool) -> Observed {
        let kind = props.get("kind").and_then(Value::as_str).unwrap_or("");
        let data = props.get("data").unwrap_or(&Value::Null);
        let chip = props.get("chip").and_then(Value::as_str).unwrap_or("");
        let session = str_field(data, "sessionID").unwrap_or("");
        let mut out = Observed::default();
        match kind {
            "session.tool.called" => {
                if !self.owns(root, session) {
                    return out;
                }
                let input = data.get("input").unwrap_or(&Value::Null);
                let tool = props.get("tool").and_then(Value::as_str).unwrap_or("");
                if input.get("background").and_then(Value::as_bool) == Some(true)
                    && self.background_calls.len() < MAX_TRACKED
                {
                    self.background_calls.insert(chip.to_owned());
                }
                if is_spawn_tool(tool) {
                    out.events.extend(self.register_spawn(session, chip, input));
                }
                if let Some(run) = self.runs.get_mut(session)
                    && !run.done
                {
                    run.tool_uses += 1;
                    if !tool.is_empty() {
                        run.last_tool = Some(tool.to_owned());
                    }
                    let chip = run.parent_tool_use_id.clone();
                    out.events.push(tag(&chip, progress(None, run)));
                }
            }
            "session.tool.progress" | "session.tool.success" | "session.tool.failed" => {
                if !self.owns(root, session) {
                    return out;
                }
                let metadata = data.get("metadata").unwrap_or(&Value::Null);
                if let Some(child) = str_field(metadata, "sessionID")
                    && self.spawns.contains_key(chip)
                {
                    out.events.extend(self.bind(chip, child, true));
                }
                if let Some(shell) = str_field(metadata, "shellID") {
                    self.shell_of_call.insert(chip.to_owned(), shell.to_owned());
                    let moved = kind == "session.tool.success"
                        && str_field(metadata, "status") == Some("running");
                    if moved || self.background_calls.contains(chip) {
                        out.events.extend(self.announce_shell(shell, chip));
                    }
                }
                if kind != "session.tool.progress" {
                    out.events
                        .extend(self.tool_settled(kind, chip, data, metadata));
                }
            }
            "session.step.started" => {
                let model = data.get("model").and_then(|m| {
                    Some(format!(
                        "{}/{}",
                        str_field(m, "providerID")?,
                        str_field(m, "id")?
                    ))
                });
                if let Some(run) = self.runs.get_mut(session)
                    && let Some(model) = model
                    && !run.done
                    && run.model.as_deref() != Some(model.as_str())
                {
                    run.model = Some(model.clone());
                    let chip = run.parent_tool_use_id.clone();
                    out.events.push(tag(&chip, progress(Some(model), run)));
                }
            }
            "session.usage.updated" => {
                if let Some(run) = self.runs.get_mut(session)
                    && let Some(tokens) = data.get("tokens")
                {
                    let n = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
                    let cache = tokens.get("cache");
                    run.total_tokens = Some(
                        n(tokens.get("input"))
                            + n(tokens.get("output"))
                            + n(tokens.get("reasoning"))
                            + n(cache.and_then(|c| c.get("read")))
                            + n(cache.and_then(|c| c.get("write"))),
                    );
                }
            }
            "session.execution.started" => {
                if session == root {
                    out.wake_started = !turn_active;
                    return out;
                }
                if let Some(run) = self.runs.get_mut(session)
                    && !run.done
                    && !run.active
                {
                    run.active = true;
                    let chip = run.parent_tool_use_id.clone();
                    out.events.push(tag(
                        &chip,
                        AgentEvent::ChildStatus {
                            status: ChildRuntimeStatus::Active,
                        },
                    ));
                }
            }
            "session.execution.succeeded" | "session.execution.failed" => {
                let Some(run) = self.runs.get_mut(session).filter(|run| !run.done) else {
                    return out;
                };
                run.active = false;
                let chip = run.parent_tool_use_id.clone();
                out.events.push(tag(&chip, progress(None, run)));
                if kind == "session.execution.failed" {
                    let message = data
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .or_else(|| data.pointer("/error/type").and_then(Value::as_str))
                        .and_then(|m| bounded(m, MAX_ERROR_CHARS))
                        .unwrap_or_else(|| "The agent failed".into());
                    out.events.push(tag(&chip, AgentEvent::Error { message }));
                }
                out.events.push(tag(
                    &chip,
                    AgentEvent::ChildStatus {
                        status: ChildRuntimeStatus::Idle,
                    },
                ));
            }
            "session.execution.interrupted" => {
                let Some(run) = self.runs.get_mut(session).filter(|run| !run.done) else {
                    return out;
                };
                run.active = false;
                let chip = run.parent_tool_use_id.clone();
                match str_field(data, "reason") {
                    // A newer prompt took over the child: still running.
                    Some("superseded") => out.events.push(tag(
                        &chip,
                        AgentEvent::ChildStatus {
                            status: ChildRuntimeStatus::Idle,
                        },
                    )),
                    reason => {
                        let error = (reason == Some("inactivity")).then(|| {
                            "Stopped: OpenCode interrupted the agent for inactivity".into()
                        });
                        out.events
                            .extend(self.settle(&chip, DoneStatus::Interrupted, error));
                    }
                }
            }
            "session.synthetic" => {
                if !self.owns(root, session) {
                    return out;
                }
                let metadata = data.get("metadata").unwrap_or(&Value::Null);
                // The completion names its child; a background child that
                // never bound falls back to its spawn's description.
                let chip = (str_field(metadata, "source") == Some("subagent"))
                    .then(|| str_field(metadata, "childID"))
                    .flatten()
                    .and_then(|child| {
                        self.runs
                            .get(child)
                            .filter(|run| run.parent_session == session)
                            .map(|run| run.parent_tool_use_id.clone())
                            .or_else(|| {
                                let description = str_field(data, "description")?;
                                self.spawn_order
                                    .iter()
                                    .find(|chip| {
                                        self.spawns.get(*chip).is_some_and(|s| {
                                            s.owner == session
                                                && s.background
                                                && !s.settled
                                                && s.child.is_none()
                                                && s.description == description
                                        })
                                    })
                                    .cloned()
                            })
                    });
                if let Some(chip) = chip {
                    let text = data.get("text").and_then(Value::as_str).unwrap_or("");
                    let (status, error) = match str_field(metadata, "state") {
                        Some("completed") => (DoneStatus::Completed, None),
                        Some("cancelled") => (DoneStatus::Interrupted, None),
                        _ => (
                            DoneStatus::Errored,
                            bounded(text, MAX_ERROR_CHARS)
                                .or_else(|| Some("The agent failed".into())),
                        ),
                    };
                    out.events.extend(self.settle(&chip, status, error));
                }
                // A completion delivered to an idle root starts its wake.
                if session == root && !turn_active {
                    self.wake_expected = true;
                }
            }
            "shell.created" => out.events.extend(self.shell_created(root, data)),
            "shell.exited" | "shell.deleted" => out.events.extend(self.shell_ended(kind, data)),
            _ => {}
        }
        out
    }

    /// The spawn tool's own result settles a foreground chip; a background
    /// spawn returns `status: "running"` and stays open.
    fn tool_settled(
        &mut self,
        kind: &str,
        chip: &str,
        data: &Value,
        metadata: &Value,
    ) -> Vec<AgentEvent> {
        self.background_calls.remove(chip);
        if !self.spawns.contains_key(chip) {
            return Vec::new();
        }
        if kind == "session.tool.success" {
            if str_field(metadata, "status") == Some("running") {
                if let Some(spawn) = self.spawns.get_mut(chip) {
                    spawn.background = true;
                }
                return self.announce_background(chip);
            }
            return self
                .settle(chip, DoneStatus::Completed, None)
                .into_iter()
                .collect();
        }
        let message = data
            .pointer("/error/message")
            .and_then(Value::as_str)
            .and_then(|m| bounded(m, MAX_ERROR_CHARS))
            .unwrap_or_else(|| "The agent failed".into());
        // OpenCode's `aborted` class is a cancellation, never a failure: a
        // Stop (the parent's tool settles before the child's own
        // `execution.interrupted`), a shutdown or a declined call. Like
        // Claude's stopped/killed/cancelled tasks it ends Interrupted; the
        // tool's own result still carries OpenCode's wording.
        let aborted = data.pointer("/error/type").and_then(Value::as_str) == Some("aborted");
        let status = if aborted || message.contains("Subagent cancelled") {
            DoneStatus::Interrupted
        } else {
            DoneStatus::Errored
        };
        let error = (status == DoneStatus::Errored).then_some(message);
        self.settle(chip, status, error).into_iter().collect()
    }

    fn commit_shell(&mut self, id: &str) -> Option<AgentEvent> {
        self.revision += 1;
        let job = self.shells.get_mut(id)?;
        job.snapshot.revision = self.revision;
        Some(AgentEvent::NativeTaskChanged {
            task: Box::new(job.snapshot.clone()),
        })
    }

    fn shell_created(&mut self, root: &str, data: &Value) -> Vec<AgentEvent> {
        let info = data.get("info").unwrap_or(&Value::Null);
        let (Some(id), Some(session)) = (
            str_field(info, "id"),
            info.get("metadata").and_then(|m| str_field(m, "sessionID")),
        ) else {
            return Vec::new();
        };
        if !self.owns(root, session) || self.shells.contains_key(id) {
            return Vec::new();
        }
        self.shells.insert(
            id.to_owned(),
            ShellJob {
                snapshot: NativeTaskSnapshot {
                    task_id: id.to_owned(),
                    kind: NativeTaskKind::Shell,
                    provider_session_id: Some(session.to_owned()),
                    description: str_field(info, "command")
                        .and_then(|c| bounded(c, MAX_DESCRIPTION_CHARS))
                        .unwrap_or_default(),
                    status: NativeTaskStatus::Running,
                    output_file: str_field(info, "file").and_then(|f| bounded(f, MAX_PATH_CHARS)),
                    ..Default::default()
                },
                announced: false,
            },
        );
        self.shell_order.push_back(id.to_owned());
        while self.shell_order.len() > MAX_TRACKED {
            let position = self
                .shell_order
                .iter()
                .position(|id| {
                    self.shells
                        .get(id)
                        .is_none_or(|job| job.snapshot.status.is_terminal())
                })
                .unwrap_or(0);
            if let Some(old) = self.shell_order.remove(position) {
                self.shells.remove(&old);
            }
        }
        // The launching call may already have asked for the background.
        let chip = self
            .shell_of_call
            .iter()
            .find(|(_, shell)| shell.as_str() == id)
            .map(|(chip, _)| chip.clone());
        match chip {
            Some(chip) if self.background_calls.contains(&chip) => self.announce_shell(id, &chip),
            _ => Vec::new(),
        }
    }

    /// A shell job is (or became) a background job: report its row.
    fn announce_shell(&mut self, id: &str, chip: &str) -> Vec<AgentEvent> {
        let Some(job) = self.shells.get_mut(id) else {
            return Vec::new();
        };
        if job.announced {
            return Vec::new();
        }
        job.announced = true;
        job.snapshot.tool_use_id = Some(chip.to_owned());
        self.commit_shell(id).into_iter().collect()
    }

    fn shell_ended(&mut self, kind: &str, data: &Value) -> Vec<AgentEvent> {
        let Some(id) = str_field(data, "id") else {
            return Vec::new();
        };
        let Some(job) = self.shells.get_mut(id) else {
            return Vec::new();
        };
        // Terminal is sticky: a later delete never rewrites an exit.
        if job.snapshot.status.is_terminal() {
            return Vec::new();
        }
        if kind == "shell.deleted" {
            job.snapshot.status = NativeTaskStatus::Stopped;
        } else {
            let exit = data.get("exit").and_then(Value::as_f64);
            match str_field(data, "status") {
                Some("exited") if exit.is_none_or(|code| code == 0.0) => {
                    job.snapshot.status = NativeTaskStatus::Completed;
                }
                Some("exited") => {
                    job.snapshot.status = NativeTaskStatus::Failed;
                    job.snapshot.error = exit.map(|code| format!("Exited with code {code}"));
                }
                Some("timeout") => {
                    job.snapshot.status = NativeTaskStatus::Failed;
                    job.snapshot.error = Some("Timed out".into());
                }
                Some("killed") => job.snapshot.status = NativeTaskStatus::Stopped,
                // Not a terminal shape this schema defines: observe only.
                _ => return Vec::new(),
            }
        }
        if !job.announced {
            // A foreground job: its tool card already tells the story.
            return Vec::new();
        }
        self.commit_shell(id).into_iter().collect()
    }
}

/// Settle every open chip locally and forward the Dones (dying runs only).
pub(super) async fn settle_children(
    children: &mut ChildTree,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    status: DoneStatus,
) {
    for event in children.settle_all(status) {
        let _ = event_tx.send(Ok(event)).await;
    }
}

/// The raw v2 observations the child tree consumes, emitted beside the
/// family payloads of the same frame (and ahead of the dispatcher's
/// sessionID gate: shell jobs carry no top-level sessionID). Only the fields
/// used here are copied, so a large tool result is never cloned.
pub(super) fn v2_observe_frame(
    kind: &str,
    data: &Value,
    tool_names: &HashMap<V2ToolKey, String>,
) -> Vec<Value> {
    let pick = |keys: &[&str]| {
        let mut picked = serde_json::Map::new();
        for key in keys {
            if let Some(value) = data.get(*key) {
                picked.insert((*key).to_owned(), value.clone());
            }
        }
        Value::Object(picked)
    };
    let (picked, chip, tool) = match kind {
        "session.tool.called" => (
            pick(&["sessionID", "input"]),
            Some(v2_tool_chip_id(data)),
            tool_names.get(&v2_tool_key(data)).cloned(),
        ),
        "session.tool.progress" | "session.tool.success" | "session.tool.failed" => {
            // Only the binding keys of the metadata (an edit's metadata can
            // carry whole diffs) and the error's own message.
            let metadata = data.get("metadata").unwrap_or(&Value::Null);
            let mut keys = serde_json::Map::new();
            for key in ["sessionID", "shellID", "status"] {
                if let Some(value) = metadata.get(key).filter(|v| v.is_string()) {
                    keys.insert(key.to_owned(), value.clone());
                }
            }
            if kind == "session.tool.progress" && keys.is_empty() {
                return Vec::new();
            }
            let mut picked = pick(&["sessionID"]);
            picked["metadata"] = Value::Object(keys);
            if let Some(error) = data.get("error").filter(|error| error.is_object()) {
                picked["error"] = json!({
                    "type": error.get("type").filter(|kind| kind.is_string()),
                    "message": error
                        .get("message")
                        .and_then(Value::as_str)
                        .and_then(|m| bounded(m, MAX_ERROR_CHARS)),
                });
            }
            (picked, Some(v2_tool_chip_id(data)), None)
        }
        "session.step.started" => (pick(&["sessionID", "model"]), None, None),
        "session.usage.updated" => (pick(&["sessionID", "tokens"]), None, None),
        "session.execution.started"
        | "session.execution.succeeded"
        | "session.execution.failed"
        | "session.execution.interrupted" => {
            let mut picked = pick(&["sessionID", "reason"]);
            if let Some(error) = data.get("error") {
                picked["error"] = json!({
                    "type": error.get("type"),
                    "message": error
                        .get("message")
                        .and_then(Value::as_str)
                        .and_then(|m| bounded(m, MAX_ERROR_CHARS)),
                });
            }
            (picked, None, None)
        }
        "session.synthetic" => {
            let mut picked = pick(&["sessionID", "metadata", "description"]);
            if let Some(text) = data
                .get("text")
                .and_then(Value::as_str)
                .and_then(|t| bounded(t, MAX_SYNTHETIC_CHARS))
            {
                picked["text"] = json!(text);
            }
            (picked, None, None)
        }
        // 2.0.22 delivers a background completion as a synthetic INBOX item
        // (`session.inbox.enqueued{item: {type: "synthetic", payload}}`),
        // observed live; read it exactly like `session.synthetic`.
        "session.inbox.enqueued" => {
            let Some(payload) = data
                .get("item")
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("synthetic"))
                .and_then(|item| item.get("payload"))
            else {
                return Vec::new();
            };
            let mut picked = pick(&["sessionID"]);
            if let Some(metadata) = payload.get("metadata") {
                picked["metadata"] = metadata.clone();
            }
            if let Some(text) = payload
                .get("text")
                .and_then(Value::as_str)
                .and_then(|t| bounded(t, MAX_SYNTHETIC_CHARS))
            {
                picked["text"] = json!(text);
            }
            return vec![json!({
                "type": OBSERVE,
                "properties": {
                    "kind": "session.synthetic",
                    "chip": Value::Null,
                    "tool": Value::Null,
                    "data": picked,
                }
            })];
        }
        "shell.created" | "shell.exited" | "shell.deleted" => (data.clone(), None, None),
        _ => return Vec::new(),
    };
    vec![json!({
        "type": OBSERVE,
        "properties": {
            "kind": kind,
            "chip": chip,
            "tool": tool,
            "data": picked,
        }
    })]
}
