//! Native hook lifecycle and command-list pushes from the Claude stream.
//!
//! Frame shapes are the Claude Code 2.1.286 SDK schema (installed binary;
//! `SessionStart` frames were also captured live from an isolated
//! initialization handshake):
//!
//! - `hook_started  {hook_id, hook_name, hook_event, uuid, session_id}`
//! - `hook_progress {hook_id, hook_name, hook_event, stdout, stderr, output, …}`
//!   (`output` is cumulative; the CLI polls it once a second and emits only on change)
//! - `hook_response {…, output, stdout, stderr, exit_code?, outcome: success|error|cancelled}`
//! - `commands_changed {commands: [{name, description, argumentHint}], …}`
//!
//! `SessionStart`/`Setup` stream by default; every other hook event streams
//! only with `--include-hook-events`. A hook run becomes a "Hook" tool chip:
//! started → [`AgentEvent::ToolCall`], progress → stdout
//! [`AgentEvent::ToolActivity`], response → [`AgentEvent::ToolResult`] whose
//! first line states the outcome and its effect on Claude. Hook frames carry no
//! `parent_tool_use_id`, so they always land in the root transcript.
use std::collections::HashMap;

use luna_proto::{AgentEvent, NATIVE_HOOK_TOOL_PREFIX, SlashCommand, ToolActivityKind, ToolCall};
use serde_json::{Value, json};

/// Retained hook output per run, live tail and final result alike.
pub(crate) const MAX_HOOK_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ExtensionFrame {
    Hook(HookFrame),
    /// Full replacement command list (`commands_changed`).
    CommandsChanged(Vec<SlashCommand>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookPhase {
    Started,
    Progress,
    Response,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HookFrame {
    pub phase: HookPhase,
    pub hook_id: String,
    pub hook_name: String,
    pub hook_event: String,
    pub stdout: String,
    pub stderr: String,
    pub output: String,
    pub exit_code: Option<i64>,
    pub outcome: Option<String>,
}

/// Slash-command entries as both `initialize` and `commands_changed` carry them.
pub(crate) fn slash_commands(entries: &[Value]) -> Vec<SlashCommand> {
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?.trim();
            (!name.is_empty()).then(|| SlashCommand {
                name: name.to_owned(),
                description: entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_hint: entry
                    .get("argumentHint")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|hint| !hint.is_empty())
                    .map(str::to_owned),
                kind: Default::default(),
            })
        })
        .collect()
}

/// Decode the system subtypes this module owns; anything else is `None`.
pub(crate) fn parse_extension_frame(value: &Value) -> Option<ExtensionFrame> {
    let phase = match value.get("subtype").and_then(Value::as_str)? {
        "hook_started" => HookPhase::Started,
        "hook_progress" => HookPhase::Progress,
        "hook_response" => HookPhase::Response,
        "commands_changed" => {
            let commands = value.get("commands")?.as_array()?;
            return Some(ExtensionFrame::CommandsChanged(slash_commands(commands)));
        }
        _ => return None,
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let hook_id = text("hook_id");
    if hook_id.trim().is_empty() {
        return None;
    }
    Some(ExtensionFrame::Hook(HookFrame {
        phase,
        hook_id,
        hook_name: text("hook_name"),
        hook_event: text("hook_event"),
        stdout: text("stdout"),
        stderr: text("stderr"),
        output: text("output"),
        exit_code: value.get("exit_code").and_then(Value::as_i64),
        outcome: value
            .get("outcome")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }))
}

#[derive(Default)]
struct HookState {
    /// Cumulative output already forwarded as live activity.
    forwarded: String,
    terminal: bool,
}

/// Per-run hook bookkeeping. Events are held back while the root turn is not
/// running: before `system:init` (`SessionStarted` resets the transcript fold,
/// so a `SessionStart` chip emitted ahead of it would be wiped) and after a
/// root result (an untagged chip there would start a phantom self-continued
/// turn). Held chips flush when the root turn shows activity again, or ahead
/// of the next result. Idle-time `Notification` hooks are logged only.
#[derive(Default)]
pub(crate) struct HookTracker {
    hooks: HashMap<String, HookState>,
    pending: Vec<AgentEvent>,
    /// A root result arrived and no root activity has followed it yet.
    idle: bool,
}

impl HookTracker {
    pub fn observe(&mut self, frame: ExtensionFrame, initialized: bool) -> Vec<AgentEvent> {
        let frame = match frame {
            ExtensionFrame::CommandsChanged(commands) => {
                return vec![AgentEvent::AvailableCommands { commands }];
            }
            ExtensionFrame::Hook(frame) => frame,
        };
        // SessionEnd runs while the CLI shuts down, after the final result,
        // and an idle Notification fires between turns: a chip there would
        // reopen a finished turn. Log them only.
        if frame.hook_event == "SessionEnd" || (self.idle && frame.hook_event == "Notification") {
            tracing::debug!(
                target: "luna_harness::claude",
                hook = %frame.hook_name,
                outcome = ?frame.outcome,
                "hook observed while no root turn was running"
            );
            return Vec::new();
        }
        let events = self.events_for(frame);
        if initialized && !self.idle {
            events
        } else {
            self.pending.extend(events);
            Vec::new()
        }
    }

    /// Held-back events, in arrival order, flushed right after
    /// `SessionStarted`.
    pub fn take_pending(&mut self) -> Vec<AgentEvent> {
        self.idle = false;
        std::mem::take(&mut self.pending)
    }

    /// A root result: flush held events ahead of its `Done`, then hold
    /// until the root turn shows activity again.
    pub fn turn_ended(&mut self) -> Vec<AgentEvent> {
        self.idle = true;
        std::mem::take(&mut self.pending)
    }

    /// Held events to emit before `frame` when it is root-turn activity (a
    /// root stream/message frame or a re-init) after a root result.
    pub fn resume_before(&mut self, frame: &super::wire::Frame) -> Vec<AgentEvent> {
        use super::wire::Frame;
        let root_activity = match frame {
            Frame::StreamEvent(f) => f.parent_tool_use_id.is_none(),
            Frame::Assistant(f) | Frame::User(f) => f.parent_tool_use_id.is_none(),
            Frame::System(f) => f.subtype == "init",
            _ => false,
        };
        if !self.idle || !root_activity {
            return Vec::new();
        }
        self.idle = false;
        std::mem::take(&mut self.pending)
    }

    fn events_for(&mut self, frame: HookFrame) -> Vec<AgentEvent> {
        let id = format!("hook:{}", frame.hook_id);
        let mut out = Vec::new();
        match self.hooks.get(&frame.hook_id) {
            Some(state) if state.terminal => return out,
            Some(_) => {}
            // A response or progress without a start still gets its chip.
            None => out.push(AgentEvent::ToolCall {
                id: id.clone(),
                call: hook_call(&frame),
            }),
        }
        let state = self.hooks.entry(frame.hook_id.clone()).or_default();
        match frame.phase {
            HookPhase::Started => {}
            HookPhase::Progress => {
                let output = combined_output(&frame);
                if let Some(delta) = output.strip_prefix(state.forwarded.as_str())
                    && !delta.is_empty()
                    && state.forwarded.len() < MAX_HOOK_OUTPUT_BYTES
                {
                    let delta = bounded(delta, MAX_HOOK_OUTPUT_BYTES - state.forwarded.len());
                    out.push(AgentEvent::ToolActivity {
                        id,
                        kind: ToolActivityKind::Stdout,
                        text: delta.to_owned(),
                    });
                }
                // A rewrite that is not an extension is never re-sent.
                state.forwarded = bounded(&output, MAX_HOOK_OUTPUT_BYTES).to_owned();
            }
            HookPhase::Response => {
                state.terminal = true;
                let verdict = classify(&frame);
                let mut text = verdict.headline(&frame.hook_event);
                let body = combined_output(&frame);
                if !body.trim().is_empty() {
                    text.push_str("\n\n");
                    let kept = bounded(&body, MAX_HOOK_OUTPUT_BYTES);
                    text.push_str(kept);
                    if kept.len() < body.len() {
                        text.push_str("\n[hook output truncated]");
                    }
                }
                out.push(AgentEvent::ToolResult {
                    id,
                    is_error: verdict.is_error(),
                    output: Some(text),
                    diff: None,
                });
            }
        }
        out
    }
}

fn hook_call(frame: &HookFrame) -> ToolCall {
    let label = if frame.hook_name.trim().is_empty() {
        frame.hook_event.as_str()
    } else {
        frame.hook_name.as_str()
    };
    ToolCall::Unknown {
        name: format!("{NATIVE_HOOK_TOOL_PREFIX}{label}"),
        input: Some(json!({ "hookEvent": frame.hook_event, "hookName": frame.hook_name })),
    }
}

/// The CLI's own combined stream when present, else stdout then stderr.
fn combined_output(frame: &HookFrame) -> String {
    if !frame.output.is_empty() {
        return frame.output.clone();
    }
    match (frame.stdout.is_empty(), frame.stderr.is_empty()) {
        (_, true) => frame.stdout.clone(),
        (true, false) => frame.stderr.clone(),
        (false, false) => format!("{}\n{}", frame.stdout, frame.stderr),
    }
}

fn bounded(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Completed,
    /// A blocking decision (exit 2 or a JSON decision) with its reason.
    Blocked(Option<String>),
    /// `continue: false`: Claude stops after this hook.
    Stopped(Option<String>),
    Failed(Option<i64>),
    Cancelled,
    Unreported,
}

impl Verdict {
    fn is_error(&self) -> bool {
        !matches!(self, Self::Completed | Self::Unreported)
    }

    fn headline(&self, event: &str) -> String {
        let with_reason = |head: &str, reason: &Option<String>| match reason {
            Some(reason) => format!("{head}: {reason}"),
            None => head.to_owned(),
        };
        match self {
            Self::Completed => "Hook completed".into(),
            Self::Blocked(reason) => with_reason(
                match event {
                    "PreToolUse" => "Hook blocked the tool call",
                    "PermissionRequest" => "Hook denied the permission request",
                    "UserPromptSubmit" | "UserPromptExpansion" => "Hook blocked the prompt",
                    "Stop" | "SubagentStop" => "Hook asked Claude to keep working",
                    "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" => {
                        "Hook sent feedback to Claude"
                    }
                    _ => "Hook returned a blocking error",
                },
                reason,
            ),
            Self::Stopped(reason) => with_reason("Hook stopped Claude", reason),
            Self::Failed(Some(code)) => format!("Hook failed (exit {code}); Claude continued"),
            Self::Failed(None) => "Hook failed; Claude continued".into(),
            Self::Cancelled => "Hook cancelled (timed out or interrupted)".into(),
            Self::Unreported => "Hook finished; outcome not reported".into(),
        }
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| bounded(line, 300).to_owned())
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .and_then(first_line)
}

/// Hook JSON output (exit 0) that changes Claude's behaviour.
fn json_decision(stdout: &str) -> Option<Verdict> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    if value.get("continue").and_then(Value::as_bool) == Some(false) {
        return Some(Verdict::Stopped(string_at(&value, "/stopReason")));
    }
    if value.get("decision").and_then(Value::as_str) == Some("block") {
        return Some(Verdict::Blocked(string_at(&value, "/reason")));
    }
    let specific = value.get("hookSpecificOutput")?;
    if specific.get("permissionDecision").and_then(Value::as_str) == Some("deny") {
        return Some(Verdict::Blocked(string_at(
            specific,
            "/permissionDecisionReason",
        )));
    }
    if specific
        .pointer("/decision/behavior")
        .and_then(Value::as_str)
        == Some("deny")
    {
        return Some(Verdict::Blocked(string_at(specific, "/decision/message")));
    }
    None
}

fn classify(frame: &HookFrame) -> Verdict {
    match frame.outcome.as_deref() {
        Some("success") => json_decision(&frame.stdout).unwrap_or(Verdict::Completed),
        // Exit 2 is Claude Code's blocking-error contract; stderr is the reason.
        Some("error") if frame.exit_code == Some(2) => {
            Verdict::Blocked(first_line(&frame.stderr).or_else(|| first_line(&frame.output)))
        }
        Some("error") => Verdict::Failed(frame.exit_code),
        Some("cancelled") => Verdict::Cancelled,
        _ => Verdict::Unreported,
    }
}
