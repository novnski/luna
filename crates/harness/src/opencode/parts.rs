//! Per-part streaming state: text/reasoning/tool snapshots and deltas.

use super::*;

/// Per-part streaming state (dedup between full-part snapshots and deltas).
#[derive(Default)]
pub(super) struct PartState {
    /// "text" | "reasoning" | "tool".
    pub(super) kind: String,
    /// Bytes of part text already emitted (snapshots resend the full text).
    pub(super) emitted: usize,
    pub(super) tool_started: bool,
    pub(super) tool_done: bool,
}

/// Streaming state for one opencode session's feed (ours or a child's).
#[derive(Default)]
pub(super) struct SessionFeed {
    /// messageID → is-assistant (user prompt echoes must not render).
    pub(super) assistant_messages: HashMap<String, bool>,
    /// Parts whose message ROLE isn't known yet, replayed when it lands.
    pub(super) pending_parts: Vec<Value>,
    pub(super) parts: HashMap<String, PartState>,
}

/// A part snapshot: emit whatever text extends what already streamed, open /
/// resolve tool chips. Snapshots and deltas interleave — `emitted` (bytes of
/// part text already sent) is the dedup line between them.
pub(super) fn part_snapshot_events(
    feed: &mut SessionFeed,
    part: &Value,
    is_main: bool,
) -> Vec<AgentEvent> {
    let Some(part_id) = part.get("id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let message_id = part
        .get("messageID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "text" | "reasoning" => {
            if kind == "text" && feed.assistant_messages.get(message_id).is_none() {
                // Role unknown: hold the part instead of guessing (dedup by
                // part id — snapshots re-deliver).
                if !feed
                    .pending_parts
                    .iter()
                    .any(|p| p.get("id").and_then(Value::as_str) == Some(part_id))
                {
                    feed.pending_parts.push(part.clone());
                }
                return Vec::new();
            }
            if kind == "text" && feed.assistant_messages.get(message_id) == Some(&false) {
                // A user-role text part. On the MAIN feed it is our own
                // prompt echo (the engine already wrote the user entry);
                // on a child feed it is the message INTO the child — its
                // spawn prompt or a steer, rendered as a user entry.
                if is_main {
                    return Vec::new();
                }
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                if text.trim().is_empty() {
                    return Vec::new();
                }
                let entry = feed
                    .parts
                    .entry(part_id.to_owned())
                    .or_insert_with(|| PartState {
                        kind: kind.to_owned(),
                        ..PartState::default()
                    });
                if entry.emitted > 0 {
                    return Vec::new();
                }
                entry.emitted = text.len();
                return vec![AgentEvent::UserMessage {
                    text: text.to_owned(),
                }];
            }
            if feed.assistant_messages.get(message_id) != Some(&true) {
                // Reasoning ahead of its message.updated: hold it too.
                if kind == "reasoning" {
                    if !feed
                        .pending_parts
                        .iter()
                        .any(|p| p.get("id").and_then(Value::as_str) == Some(part_id))
                    {
                        feed.pending_parts.push(part.clone());
                    }
                }
                return Vec::new();
            }
            let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
            let entry = feed
                .parts
                .entry(part_id.to_owned())
                .or_insert_with(|| PartState {
                    kind: kind.to_owned(),
                    ..PartState::default()
                });
            let Some(suffix) = text
                .get(entry.emitted..)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            else {
                // Shorter (or mid-char) snapshot: a rewrite this decoder
                // doesn't model — drop it rather than duplicate text.
                return Vec::new();
            };
            entry.emitted = text.len();
            vec![if entry.kind == "reasoning" {
                AgentEvent::ReasoningDelta { text: suffix }
            } else {
                AgentEvent::TextDelta { text: suffix }
            }]
        }
        "tool" => {
            let tool = part.get("tool").and_then(Value::as_str).unwrap_or_default();
            let status = part
                .get("state")
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input = part
                .get("state")
                .and_then(|s| s.get("input"))
                .cloned()
                .unwrap_or(Value::Null);
            // Chips key on callID (the v2 `session:message:call` id, which
            // a spawn's child binding and settlement key on as well).
            let call_id = part
                .get("callID")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(part_id)
                .to_owned();
            let entry = feed
                .parts
                .entry(part_id.to_owned())
                .or_insert_with(|| PartState {
                    kind: "tool".to_owned(),
                    ..PartState::default()
                });
            let mut events = Vec::new();
            let has_input = input.as_object().is_some_and(|o| !o.is_empty());
            if !entry.tool_started && (has_input || matches!(status, "completed" | "error")) {
                entry.tool_started = true;
                // A spawn (`subagent`/`task`) registers with the child tree
                // from the raw v2 observation of the same frame, in any
                // session of the run (`subagents`).
                events.push(AgentEvent::ToolCall {
                    id: call_id.clone(),
                    call: oc_tool_call(tool, &input),
                });
            }
            if entry.tool_started && !entry.tool_done && matches!(status, "completed" | "error") {
                entry.tool_done = true;
                let output = part
                    .get("state")
                    .and_then(|s| {
                        s.get("output")
                            .or_else(|| s.get("error"))
                            .and_then(Value::as_str)
                    })
                    .filter(|t| !t.is_empty())
                    .map(|t| cap_text(t, OUTPUT_CAP));
                events.push(AgentEvent::ToolResult {
                    id: call_id,
                    is_error: status == "error",
                    output,
                    diff: None,
                });
            }
            events
        }
        // step-start / step-finish / snapshot / patch bookkeeping: not
        // transcript.
        _ => Vec::new(),
    }
}

/// A text delta appends to its part. Deltas follow the part's opening
/// `message.part.updated` (which fixes the kind); an unknown part defaults
/// to assistant text only when its message is known assistant.
pub(super) fn part_delta_events(
    feed: &mut SessionFeed,
    props: &Value,
    part_id: &str,
    delta: &str,
) -> Vec<AgentEvent> {
    if delta.is_empty() {
        return Vec::new();
    }
    let message_id = props
        .get("messageID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if feed.assistant_messages.get(message_id) != Some(&true) {
        return Vec::new();
    }
    let entry = feed
        .parts
        .entry(part_id.to_owned())
        .or_insert_with(|| PartState {
            kind: "text".to_owned(),
            ..PartState::default()
        });
    if entry.kind == "tool" {
        return Vec::new();
    }
    entry.emitted += delta.len();
    vec![if entry.kind == "reasoning" {
        AgentEvent::ReasoningDelta {
            text: delta.to_owned(),
        }
    } else {
        AgentEvent::TextDelta {
            text: delta.to_owned(),
        }
    }]
}
