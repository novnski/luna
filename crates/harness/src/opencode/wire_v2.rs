//! The 2.x event vocabulary rewritten into the internal 1.x-shaped bus
//! payloads the run loop consumes.

use super::*;

/// Rewrite one 2.x `/api/event` frame into the 1.x-shaped bus payloads
/// (`{type, properties}`) the session loop consumes, so the whole turn
/// engine — feeds, settle, retries, subagents — stays wire-agnostic. The
/// 2.x frame is `{id, type, data: {sessionID, assistantMessageID, ..}}`
/// (all shapes captured live from a 2.0.3 server); the names and shapes
/// of the 2.x event vocabulary:
/// - turn lifecycle: `session.execution.started` (busy) and
///   `.succeeded`/`.interrupted`/`.failed` (idle; failed carries the error).
/// - per step: `session.step.started` fixes the assistant message id its
///   items stream under.
/// - streaming items: `session.text.delta` (+ started/ended snapshots) and
///   `session.reasoning.started/.ended`; `ordinal` numbers the item within
///   the step — the synthetic part id.
/// - tools: `session.tool.input.started` (carries the NAME),
///   `.input.ended` (args as text), `.called` (args as object),
///   `.success` (content array) / `.error`.
/// - usage: `session.usage.updated` with the cumulative token totals.
///
/// `tool_names` tracks pending calls by session, message, and provider call id.
pub(super) type V2ToolKey = (String, String, String);
pub(super) const MAX_PENDING_V2_TOOLS: usize = 4096;

pub(super) fn normalize_v2_frame(
    event: Value,
    tool_names: &mut HashMap<V2ToolKey, String>,
) -> Vec<Value> {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    if data
        .get("sessionID")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Vec::new();
    }
    if matches!(
        kind,
        "session.execution.succeeded"
            | "session.execution.interrupted"
            | "session.execution.failed"
    ) {
        tool_names.retain(|(owner, _, _), _| {
            Some(owner.as_str()) != data.get("sessionID").and_then(Value::as_str)
        });
    }
    // One helper per event family, so each family evolves on its own. A
    // helper answers `None` for kinds outside its family.
    v2_session_frames(kind, &data)
        .or_else(|| v2_stream_frames(kind, &data))
        .or_else(|| v2_tool_frames(kind, &data, tool_names))
        .or_else(|| v2_request_frames(kind, &data))
        .or_else(|| v2_other_frames(kind, &data))
        .unwrap_or_default()
}

fn v2_session(data: &Value) -> Value {
    data.get("sessionID").cloned().unwrap_or(Value::Null)
}

fn v2_message(data: &Value) -> Value {
    data.get("assistantMessageID")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Pending-tool key: session, assistant message and provider call id.
fn v2_tool_key(data: &Value) -> V2ToolKey {
    (
        data.get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        data.get("assistantMessageID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        data.get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    )
}

/// Session and execution lifecycle: busy, terminals, steps, child sessions.
fn v2_session_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        "session.execution.started" => vec![json!({
            "type": "session.status",
            "properties": { "sessionID": v2_session(data), "status": { "type": "busy" } }
        })],
        "session.execution.interrupted" => vec![json!({
            "type": "session.interrupted",
            "properties": { "sessionID": v2_session(data) }
        })],
        "session.execution.succeeded" => vec![json!({
            "type": "session.idle",
            "properties": { "sessionID": v2_session(data) }
        })],
        "session.execution.failed" => {
            vec![
                v2_error_payload(data),
                json!({
                    "type": "session.idle",
                    "properties": { "sessionID": v2_session(data) }
                }),
            ]
        }
        "session.step.failed" => {
            // An interrupt surfaces as step.failed{aborted}; the terminal
            // execution.interrupted frame settles the turn. Any other
            // failure is a step-level provider error: surface it as a chip,
            // the turn itself keeps going (execution.* decides its end).
            if data.pointer("/error/type").and_then(Value::as_str) == Some("aborted") {
                return Some(Vec::new());
            }
            let mut warning = v2_error_payload(data);
            warning["type"] = json!("session.warning");
            vec![warning]
        }
        // OC-12: a session deleted outside LUNA (another client, or the
        // agent's own tool) ends its turn with an actionable error rather
        // than a stall. Non-root sessions' frames are ignored by the bus.
        "session.deleted" => {
            let session = data.get("sessionID").and_then(Value::as_str).unwrap_or("");
            vec![
                json!({
                    "type": "session.error",
                    "properties": {
                        "sessionID": v2_session(data),
                        "error": {
                            "name": "SessionDeleted",
                            "data": { "message": history::deleted_session_message(session) },
                        },
                    }
                }),
                json!({
                    "type": "session.idle",
                    "properties": { "sessionID": v2_session(data) }
                }),
            ]
        }
        "session.step.started" => vec![json!({
            "type": "message.updated",
            "properties": {
                "info": { "sessionID": v2_session(data), "id": v2_message(data), "role": "assistant" }
            }
        })],
        "session.created" => {
            let Some(id) = data.get("sessionID").and_then(Value::as_str) else {
                return Some(Vec::new());
            };
            vec![json!({
                "type": "session.created",
                "properties": {
                    "info": {
                        "id": id,
                        "parentID": data.get("parentID").cloned().unwrap_or(Value::Null),
                        "title": data
                            .get("title")
                            .or_else(|| data.get("slug"))
                            .cloned()
                            .unwrap_or(Value::Null),
                    }
                }
            })]
        }
        _ => return None,
    };
    Some(frames)
}

/// Streamed assistant items: text and reasoning snapshots and deltas.
fn v2_stream_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        "session.text.started" | "session.text.ended" => {
            vec![v2_stream_part(
                data,
                "text",
                data.get("text").cloned().unwrap_or(json!("")),
            )]
        }
        "session.text.delta" | "session.reasoning.delta" => vec![json!({
            "type": "message.part.delta",
            "properties": {
                "sessionID": v2_session(data),
                "messageID": v2_message(data),
                "partID": v2_part_id(data, if kind == "session.reasoning.delta" { 'r' } else { 't' }),
                "field": "text",
                "delta": data.get("delta").cloned().unwrap_or(json!("")),
            }
        })],
        "session.reasoning.started" | "session.reasoning.ended" => vec![v2_stream_part(
            data,
            "reasoning",
            data.get("text").cloned().unwrap_or(json!("")),
        )],
        _ => return None,
    };
    Some(frames)
}

/// Tool calls: the name arrives on `input.started`; later frames carry the
/// provider call id alone.
fn v2_tool_frames(
    kind: &str,
    data: &Value,
    tool_names: &mut HashMap<V2ToolKey, String>,
) -> Option<Vec<Value>> {
    let frames = match kind {
        "session.tool.input.started" => {
            let id = data.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = data.get("name").and_then(Value::as_str).unwrap_or_default();
            tool_names.insert(v2_tool_key(data), name.to_owned());
            vec![v2_tool_part(
                data,
                id,
                name,
                &json!({ "status": "pending" }),
            )]
        }
        "session.tool.called" => {
            let id = data.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tool_names
                .get(&v2_tool_key(data))
                .map(String::as_str)
                .unwrap_or_default();
            let state = json!({
                "status": "running",
                "input": data.get("input").cloned().unwrap_or(json!({})),
            });
            vec![v2_tool_part(data, id, name, &state)]
        }
        "session.tool.success" => {
            let id = data.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tool_names.remove(&v2_tool_key(data)).unwrap_or_default();
            let output = data
                .get("content")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            vec![v2_tool_part(
                data,
                id,
                &name,
                &json!({ "status": "completed", "output": output }),
            )]
        }
        "session.tool.failed" | "session.tool.error" => {
            let id = data.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tool_names.remove(&v2_tool_key(data)).unwrap_or_default();
            let error = data.get("error").cloned().unwrap_or(Value::Null);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| error.as_str().map(str::to_owned))
                .unwrap_or_default();
            vec![v2_tool_part(
                data,
                id,
                &name,
                &json!({ "status": "error", "error": message }),
            )]
        }
        _ => return None,
    };
    Some(frames)
}

/// Requests that wait on the user: permissions (and, later, forms).
fn v2_request_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        // Same event name as 1.x; preserve permission details for the reply
        // and any interactive approval. Observed live on 2.0.3 when a tool
        // reaches outside the workspace.
        "permission.asked" => {
            if data.get("id").and_then(Value::as_str).is_none() {
                return Some(Vec::new());
            }
            vec![json!({
                "type": "permission.asked",
                "properties": data
            })]
        }
        _ => return None,
    };
    Some(frames)
}

/// Everything else: usage totals.
fn v2_other_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        "session.usage.updated" => {
            let tokens = data.get("tokens").cloned().unwrap_or(Value::Null);
            if tokens.is_null() {
                return Some(Vec::new());
            }
            // Cumulative totals keyed to a synthetic message: registers
            // Usage + ContextUsage exactly like the 1.x assistant
            // message.updated did. (No providerID/modelID on this frame —
            // the context window is dropped.)
            vec![json!({
                "type": "message.updated",
                "properties": {
                    "info": {
                        "sessionID": v2_session(data),
                        "id": "usage",
                        "role": "assistant",
                        "tokens": tokens,
                    }
                }
            })]
        }
        _ => return None,
    };
    Some(frames)
}

/// The 2.x error shape (`{type, message}`) folded into the 1.x
/// `session.error` properties (`{name, data: {message}}`). 2.x provider
/// failures can carry an EMPTY message (observed live: `provider.auth`
/// with `message: ""`) — fall back to the type so the chip isn't blank.
pub(super) fn v2_error_payload(data: &Value) -> Value {
    let error = data.get("error").cloned().unwrap_or(Value::Null);
    let name = error.get("type").and_then(Value::as_str).unwrap_or("");
    let message = match error.get("message").and_then(Value::as_str) {
        Some(message) if !message.is_empty() => message,
        _ => name,
    };
    json!({
        "type": "session.error",
        "properties": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "error": {
                "name": name,
                "data": { "message": message },
            },
        }
    })
}

/// Synthetic part id for a streamed 2.x text/reasoning item: message id +
/// kind + ordinal (text and reasoning number separately).
pub(super) fn v2_part_id(data: &Value, kind: char) -> String {
    let message = data
        .get("assistantMessageID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let ordinal = data.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
    format!("{message}:{kind}{ordinal}")
}

/// A text/reasoning item snapshot in the 1.x part shape.
pub(super) fn v2_stream_part(data: &Value, part_type: &str, text: Value) -> Value {
    let kind_char = if part_type == "text" { 't' } else { 'r' };
    json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "id": v2_part_id(data, kind_char),
            "messageID": data.get("assistantMessageID").cloned().unwrap_or(Value::Null),
            "type": part_type,
            "text": text,
        }}
    })
}

/// A tool frame in the 1.x tool-part shape (`callID` keys the chip).
pub(super) fn v2_tool_part(data: &Value, id: &str, name: &str, state: &Value) -> Value {
    // Provider call ids can repeat across assistant messages. Both the feed's
    // state key and the emitted chip id must keep those calls distinct.
    let id = format!(
        "{}:{}:{id}",
        data.get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        data.get("assistantMessageID")
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "messageID": data.get("assistantMessageID").cloned().unwrap_or(Value::Null),
            "id": id,
            "callID": id,
            "type": "tool",
            "tool": name,
            "state": state,
        }}
    })
}
