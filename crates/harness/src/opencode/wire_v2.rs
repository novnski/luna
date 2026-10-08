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
/// - delivery: `session.inbox.enqueued/.delivered/.cancelled/.delivery.changed`
///   (the inbox id is the caller's `msg_…` prompt id), plus
///   `session.retry.scheduled` and `session.step.ended` finish reasons.
/// - usage, model and compaction (`usage` module): `luna.*` payloads from
///   `session.step.started/.ended`, the execution terminals,
///   `session.usage.updated` (running totals), `session.model.selected`
///   and `session.compaction.*`; catalog updates arrive as global events.
///
/// `tool_names` tracks pending calls by session, message, and provider call id.
pub(super) type V2ToolKey = (String, String, String);
pub(super) const MAX_PENDING_V2_TOOLS: usize = 4096;

pub(super) fn normalize_v2_frame(
    event: Value,
    tool_names: &mut HashMap<V2ToolKey, String>,
) -> Vec<Value> {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    // Global (session-less) catalog updates invalidate the run's catalog.
    if let Some(frames) = v2_global_frames(kind) {
        return frames;
    }
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    // Native subagent and background-job observations (OC-11) ride beside
    // the family payloads. Computed first: tool names are still pending, and
    // shell jobs carry no top-level sessionID.
    let observed = v2_observe_frame(kind, &data, tool_names);
    // `form.created` carries its owner inside the form (`data.form.sessionID`,
    // possibly the MCP `"global"` sentinel), so it routes before the gate.
    if kind == "form.created" {
        let mut frames = v2_form_created(&data);
        frames.extend(observed);
        return frames;
    }
    if data
        .get("sessionID")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return observed;
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
    // Accounting rides AHEAD of the family frames for the kinds other
    // families also claim (step and execution lifecycle), so a turn's
    // usage is recorded before its idle frame settles the turn.
    let mut frames = v2_accounting_frames(kind, &data);
    // One helper per event family, so each family evolves on its own. A
    // helper answers `None` for kinds outside its family.
    frames.extend(
        v2_session_frames(kind, &data)
            .or_else(|| v2_inbox_frames(kind, &data))
            .or_else(|| v2_stream_frames(kind, &data))
            .or_else(|| v2_tool_frames(kind, &data, tool_names))
            .or_else(|| v2_request_frames(kind, &data))
            .or_else(|| v2_other_frames(kind, &event, &data))
            .unwrap_or_default(),
    );
    frames.extend(observed);
    frames
}

fn v2_session(data: &Value) -> Value {
    data.get("sessionID").cloned().unwrap_or(Value::Null)
}

fn v2_message(data: &Value) -> Value {
    data.get("assistantMessageID")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The chip id of a v2 tool frame: provider call ids can repeat across
/// assistant messages and sessions (a child's call never collides with its
/// parent's), so the id is `session:message:call`.
pub(super) fn v2_tool_chip_id(data: &Value) -> String {
    format!(
        "{}:{}:{}",
        data.get("sessionID")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        data.get("assistantMessageID")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        data.get("id").and_then(Value::as_str).unwrap_or_default()
    )
}

/// Pending-tool key: session, assistant message and provider call id.
pub(super) fn v2_tool_key(data: &Value) -> V2ToolKey {
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

/// Session and execution lifecycle: busy, terminals, steps, child sessions,
/// retries and finish reasons.
fn v2_session_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        "session.execution.started" => vec![json!({
            "type": "session.status",
            "properties": { "sessionID": v2_session(data), "status": { "type": "busy" } }
        })],
        "session.execution.interrupted" => {
            // The reason (user, shutdown, superseded, inactivity) rides
            // along: only a user interrupt is a plain Stop.
            let mut properties = json!({ "sessionID": v2_session(data) });
            if let Some(reason) = data.get("reason").filter(|reason| !reason.is_null()) {
                properties["reason"] = reason.clone();
            }
            vec![json!({ "type": "session.interrupted", "properties": properties })]
        }
        "session.execution.succeeded" => vec![json!({
            "type": "session.idle",
            "properties": { "sessionID": v2_session(data) }
        })],
        "session.execution.failed" => {
            // The terminal reason, then a frame that always settles Errored:
            // an execution failure after streamed content is still a
            // failed turn.
            vec![
                v2_error_payload(data),
                json!({
                    "type": "session.failed",
                    "properties": { "sessionID": v2_session(data) }
                }),
            ]
        }
        "session.step.failed" => {
            // An interrupt surfaces as step.failed{aborted}; the terminal
            // execution.interrupted frame settles the turn. Any other
            // failure is a step-level provider warning: surface it transiently,
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
        // Only the finish reasons a user must see: the reply hit the output
        // limit or a content filter (OC-07 step 6).
        "session.step.ended" => match data.get("finish").and_then(Value::as_str) {
            Some(finish @ ("length" | "content-filter")) => vec![json!({
                "type": "session.finish",
                "properties": {
                    "sessionID": v2_session(data),
                    "messageID": v2_message(data),
                    "finish": finish,
                }
            })],
            _ => Vec::new(),
        },
        // v2 caps provider retries itself; each scheduled retry is a
        // transient notice, never an error (OC-07 step 5).
        "session.retry.scheduled" => vec![json!({
            "type": "session.retry",
            "properties": {
                "sessionID": v2_session(data),
                "attempt": data.get("attempt").cloned().unwrap_or(Value::Null),
                "at": data.get("at").cloned().unwrap_or(Value::Null),
                "message": v2_error_message(data.get("error").unwrap_or(&Value::Null)),
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
                        // The child's agent: its permission ceiling is
                        // compiled against that agent's own rules.
                        "agent": data.get("agent").cloned().unwrap_or(Value::Null),
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

/// The prompt inbox: receipt evidence for LUNA's own queue (OC-07). The
/// inbox id equals the `msg_…` id the prompt was posted with. A synthetic
/// input (a background-subagent completion, another client's text) is an
/// inbox item LUNA never posts itself.
fn v2_inbox_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let internal = match kind {
        "session.inbox.enqueued" => "inbox.enqueued",
        "session.inbox.delivered" => "inbox.delivered",
        "session.inbox.cancelled" => "inbox.cancelled",
        "session.inbox.delivery.changed" => "inbox.delivery",
        "session.synthetic" => {
            return Some(vec![json!({
                "type": "session.synthetic",
                "properties": {
                    "sessionID": v2_session(data),
                    "description": data.get("description").cloned().unwrap_or(Value::Null),
                }
            })]);
        }
        _ => return None,
    };
    let Some(inbox_id) = data.get("inboxID").and_then(Value::as_str) else {
        return Some(Vec::new());
    };
    let item = data.get("item").unwrap_or(&Value::Null);
    Some(vec![json!({
        "type": internal,
        "properties": {
            "sessionID": v2_session(data),
            "inboxID": inbox_id,
            "delivery": data
                .get("delivery")
                .or_else(|| item.get("delivery"))
                .cloned()
                .unwrap_or(Value::Null),
            "itemType": item.get("type").cloned().unwrap_or(Value::Null),
        }
    })])
}

/// Streamed assistant items: text and reasoning snapshots and deltas.
fn v2_stream_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        "session.text.started"
        | "session.text.ended"
        | "session.reasoning.started"
        | "session.reasoning.ended" => {
            let mut part = v2_stream_part(
                data,
                if kind.starts_with("session.reasoning.") {
                    "reasoning"
                } else {
                    "text"
                },
                data.get("text").cloned().unwrap_or(json!("")),
            );
            if kind.ends_with(".ended") {
                part["properties"]["part"]["completed"] = json!(true);
            }
            vec![part]
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
        _ => return None,
    };
    Some(frames)
}

/// Tool calls: the name arrives on `input.started`; later frames carry the
/// provider call id alone. Terminal frames keep the whole native result
/// (`content`, `metadata`, `resultState`, `executed`, the error's type) for
/// the result projection (`tool_output`); input deltas and progress become
/// internal side-channel payloads (`tool.input.delta`, `tool.progress`).
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
        // Arguments being generated: a liveness signal (the text itself is
        // only diagnostic; `called` carries the parsed arguments).
        "session.tool.input.delta" => vec![json!({
            "type": "tool.input.delta",
            "properties": {
                "sessionID": v2_session(data),
                "messageID": v2_message(data),
                "callID": v2_scoped_call_id(data),
                "bytes": data.get("delta").and_then(Value::as_str).map_or(0, str::len),
            }
        })],
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
        "session.tool.progress" => vec![json!({
            "type": "tool.progress",
            "properties": {
                "sessionID": v2_session(data),
                "messageID": v2_message(data),
                "callID": v2_scoped_call_id(data),
                "tool": tool_names.get(&v2_tool_key(data)).cloned().unwrap_or_default(),
                "metadata": data.get("metadata").cloned().unwrap_or(Value::Null),
            }
        })],
        "session.tool.success" => {
            let id = data.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tool_names.remove(&v2_tool_key(data)).unwrap_or_default();
            let content = data.get("content").cloned().unwrap_or(json!([]));
            let output = content
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let mut state = json!({ "status": "completed", "output": output, "content": content });
            v2_copy_result_fields(data, &mut state);
            vec![v2_tool_part(data, id, &name, &state)]
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
            let mut state = json!({ "status": "error", "error": message });
            if let Some(error_type) = error.get("type").and_then(Value::as_str) {
                state["errorType"] = json!(error_type);
            }
            if let Some(content) = data.get("content").filter(|c| c.is_array()) {
                state["content"] = content.clone();
            }
            v2_copy_result_fields(data, &mut state);
            vec![v2_tool_part(data, id, &name, &state)]
        }
        _ => return None,
    };
    Some(frames)
}

/// The optional native result fields a terminal tool frame carries.
fn v2_copy_result_fields(data: &Value, state: &mut Value) {
    for key in ["metadata", "resultState", "executed"] {
        if let Some(value) = data.get(key).filter(|v| !v.is_null()) {
            state[key] = value.clone();
        }
    }
}

/// The chip id of a 2.x tool call: provider call ids can repeat across
/// assistant messages, so the id is scoped by session and message.
pub(super) fn v2_scoped_call_id(data: &Value) -> String {
    // One formula for every chip id: the child binding (OC-11) and the LUNA
    // tool fence key on it too.
    v2_tool_chip_id(data)
}

/// Requests that wait on the user: permission asks and their settlement,
/// form settlement (`form.created` routes before the session gate), and the
/// `session.permissions` readback of applied rules. All pass through with
/// their v2 fields (`oc2:schema/src/permission.ts`, `form.ts`).
fn v2_request_frames(kind: &str, data: &Value) -> Option<Vec<Value>> {
    let frames = match kind {
        // `{id, sessionID, action, resources, save?, metadata?, source?,
        // message?}` — read by the approval card.
        "permission.asked" => {
            if data.get("id").and_then(Value::as_str).is_none() {
                return Some(Vec::new());
            }
            vec![json!({
                "type": "permission.asked",
                "properties": data
            })]
        }
        // `permission.replied {sessionID, requestID, reply}`,
        // `form.replied {id, sessionID, answer}`, `form.cancelled {id,
        // sessionID}`, `session.permissions {sessionID, permissions}`.
        "permission.replied" | "form.replied" | "form.cancelled" | "session.permissions" => {
            vec![json!({ "type": kind, "properties": data })]
        }
        _ => return None,
    };
    Some(frames)
}

/// `form.created {form}` → `{type, properties: {sessionID: <owner>, form}}`
/// (the owner may be the `"global"` sentinel for MCP elicitation).
fn v2_form_created(data: &Value) -> Vec<Value> {
    let Some(form) = data.get("form").filter(|form| {
        form.get("id").and_then(Value::as_str).is_some()
            && form
                .get("sessionID")
                .and_then(Value::as_str)
                .is_some_and(|owner| !owner.is_empty())
    }) else {
        return Vec::new();
    };
    vec![json!({
        "type": "form.created",
        "properties": { "sessionID": form["sessionID"], "form": form }
    })]
}

/// Everything else: running usage totals, model selection and the
/// compaction lifecycle (`usage` module). The totals are cumulative: cost
/// and accounting only, never context occupancy or a synthetic message.
fn v2_other_frames(kind: &str, event: &Value, data: &Value) -> Option<Vec<Value>> {
    v2_usage_frames(kind, event, data)
}

/// The 2.x error shape (`{type, message, status?, response?: {body}}`)
/// folded into the 1.x `session.error` properties (`{name, data:
/// {message}}`).
pub(super) fn v2_error_payload(data: &Value) -> Value {
    let error = data.get("error").cloned().unwrap_or(Value::Null);
    let name = error.get("type").and_then(Value::as_str).unwrap_or("");
    json!({
        "type": "session.error",
        "properties": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "error": {
                "name": name,
                "data": { "message": v2_error_message(&error) },
            },
        }
    })
}

/// The human text of a v2 `SessionStructuredError`. 2.x provider failures
/// can carry an EMPTY message (observed live: `provider.auth` with
/// `message: ""`): fall back to the type so the chip isn't blank. The type,
/// the HTTP status and a bounded response body are kept when present.
pub(super) fn v2_error_message(error: &Value) -> String {
    let name = error.get("type").and_then(Value::as_str).unwrap_or("");
    let mut message = match error.get("message").and_then(Value::as_str) {
        Some(message) if !message.is_empty() => message.to_owned(),
        _ => name.to_owned(),
    };
    let status = error.get("status").and_then(Value::as_u64);
    let mut detail: Vec<String> = Vec::new();
    if !name.is_empty() && name != message {
        detail.push(name.to_owned());
    }
    if let Some(status) = status {
        detail.push(format!("HTTP {status}"));
    }
    if !detail.is_empty() {
        message.push_str(&format!(" ({})", detail.join(", ")));
    }
    if let Some(body) = error
        .pointer("/response/body")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|body| !body.is_empty())
    {
        message.push_str(&format!(": {}", truncate_body(body)));
    }
    message
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
pub(super) fn v2_tool_part(data: &Value, _id: &str, name: &str, state: &Value) -> Value {
    // Provider call ids can repeat across assistant messages. Both the feed's
    // state key and the emitted chip id must keep those calls distinct.
    let id = v2_tool_chip_id(data);
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
