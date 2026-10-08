//! Reconnect recovery (OC-08): what the run loop reads back after the SSE
//! bus dropped frames.
//!
//! OpenCode 2.0.22's experimental durable log (`…/log?after=<seq>`) keeps
//! only its watermark, so the bus uses it to detect a gap ([`ReplayGap`])
//! and the loop fills the gap from the projected messages: everything after
//! the turn's anchor message is re-emitted as synthesized v2 frames through
//! the ordinary normalizer, where part snapshots and tool states dedupe
//! against what already streamed. Pending permissions and forms are re-read
//! and re-injected once; the session record settles a turn whose terminal
//! was lost.

use super::*;

/// Message pages read backwards looking for the anchor (50 per page).
const READBACK_PAGES: usize = 6;
const READBACK_PAGE_SIZE: u32 = 50;

/// Synthesized v2 frames (oldest first) for every message after `anchor`
/// (the anchor itself included when it is a user message, so a missed
/// delivery of it is recovered too). Without an anchor, `known` (message
/// ids the bus already showed) bounds the window. `Ok(None)`: the anchor
/// was not found within the pages read, so nothing is replayed rather than
/// re-emitting older turns.
/// A parked run sets `after_idle`: its completed turn through the first
/// idle marker is skipped, leaving only background completions and wakes.
pub(super) async fn readback_frames(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    anchor: Option<&str>,
    known: &HashSet<String>,
    after_idle: bool,
) -> Result<Option<Vec<Value>>, HarnessError> {
    let mut newer: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..READBACK_PAGES {
        let path = match &cursor {
            Some(cursor) => format!(
                "/api/session/{session_id}/message?limit={READBACK_PAGE_SIZE}&cursor={cursor}"
            ),
            None => {
                format!("/api/session/{session_id}/message?order=desc&limit={READBACK_PAGE_SIZE}")
            }
        };
        let page = server.get_json(&path, dir).await?;
        let items = page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in items {
            let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let is_anchor = anchor == Some(id);
            let is_known = anchor.is_none() && known.contains(id);
            if is_anchor || is_known {
                // The boundary message: a user message may be our own
                // undelivered-looking prompt; an assistant message may be
                // the one that was streaming when the bus dropped.
                newer.push(item);
                newer.reverse();
                if after_idle {
                    let Some(idle) = newer.iter().position(|message| {
                        message.get("type").and_then(Value::as_str) == Some("idle")
                    }) else {
                        return Ok(None);
                    };
                    newer.drain(..=idle);
                }
                return Ok(Some(message_frames(session_id, &newer)));
            }
            newer.push(item);
        }
        cursor = page
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

/// Projected v2 messages (oldest first) as the v2 frames that produced
/// them. Text and reasoning ordinals count per kind inside a message, as
/// the live stream numbers them, so the synthetic part ids line up.
pub(super) fn message_frames(session_id: &str, messages: &[Value]) -> Vec<Value> {
    let frame = |kind: &str, data: Value| json!({ "type": kind, "data": data, "replayed": true });
    let mut frames = Vec::new();
    let mut execution_open = false;
    for message in messages {
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "user" | "synthetic" => {
                if message.get("type").and_then(Value::as_str) == Some("synthetic") {
                    frames.push(frame(
                        "session.synthetic",
                        json!({
                            "sessionID": session_id,
                            "metadata": message.get("metadata"),
                            "text": message.get("text"),
                            "description": message.get("description"),
                        }),
                    ));
                }
                frames.push(frame(
                    "session.inbox.delivered",
                    json!({ "sessionID": session_id, "inboxID": id }),
                ));
            }
            "assistant" => {
                if !execution_open {
                    // A wake may have started and finished entirely during
                    // the gap. Its assistant history proves that execution
                    // existed, so restore the boundary before its content.
                    frames.push(frame(
                        "session.execution.started",
                        json!({ "sessionID": session_id }),
                    ));
                    execution_open = true;
                }
                let base = |extra: Value| {
                    let mut data = json!({ "sessionID": session_id, "assistantMessageID": id });
                    if let (Some(data), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
                        data.extend(extra.clone());
                    }
                    data
                };
                frames.push(frame("session.step.started", base(json!({}))));
                let (mut text, mut reasoning) = (0u64, 0u64);
                for item in message
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "text" => {
                            frames.push(frame(
                                "session.text.ended",
                                base(json!({
                                    "ordinal": text,
                                    "text": item.get("text").cloned().unwrap_or(json!("")),
                                })),
                            ));
                            text += 1;
                        }
                        "reasoning" => {
                            frames.push(frame(
                                "session.reasoning.ended",
                                base(json!({
                                    "ordinal": reasoning,
                                    "text": item.get("text").cloned().unwrap_or(json!("")),
                                })),
                            ));
                            reasoning += 1;
                        }
                        "tool" => {
                            let call = item.get("id").cloned().unwrap_or(Value::Null);
                            let state = item.get("state").unwrap_or(&Value::Null);
                            frames.push(frame(
                                "session.tool.input.started",
                                base(json!({
                                    "id": call,
                                    "name": item.get("name").cloned().unwrap_or(Value::Null),
                                })),
                            ));
                            if let Some(input) =
                                state.get("input").filter(|input| input.is_object())
                            {
                                frames.push(frame(
                                    "session.tool.called",
                                    base(json!({ "id": call, "input": input })),
                                ));
                            }
                            match state.get("status").and_then(Value::as_str) {
                                Some("completed") => frames.push(frame(
                                    "session.tool.success",
                                    base(json!({
                                        "id": call,
                                        "content": state.get("content").cloned().unwrap_or(json!([])),
                                        "metadata": state.get("metadata").cloned().unwrap_or(Value::Null),
                                    })),
                                )),
                                Some("error") => frames.push(frame(
                                    "session.tool.failed",
                                    base(json!({
                                        "id": call,
                                        "error": state.get("error").cloned().unwrap_or(Value::Null),
                                    })),
                                )),
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(finish) = message.get("finish").filter(|finish| !finish.is_null()) {
                    frames.push(frame(
                        "session.step.ended",
                        base(json!({ "finish": finish })),
                    ));
                }
            }
            // The idle marker closes the busy period: its outcome is the
            // execution terminal the bus missed.
            "idle" => {
                execution_open = false;
                match message.get("outcome").and_then(Value::as_str) {
                Some("succeeded") => frames.push(frame(
                    "session.execution.succeeded",
                    json!({ "sessionID": session_id }),
                )),
                Some("failed") => frames.push(frame(
                    "session.execution.failed",
                    json!({
                        "sessionID": session_id,
                        "error": {
                            "type": "unknown",
                            "message": "OpenCode recorded the turn as failed while LUNA's event stream was reconnecting",
                        },
                    }),
                )),
                Some("interrupted") => frames.push(frame(
                    "session.execution.interrupted",
                    json!({ "sessionID": session_id, "reason": "unknown" }),
                )),
                _ => {}
            }
            }
            _ => {}
        }
    }
    frames
}

/// A pending permission or form request read back after a reconnect.
pub(super) struct PendingRequest {
    pub(super) id: String,
    pub(super) session: String,
    /// The raw v2 frame that would have announced it.
    pub(super) frame: Value,
}

/// Each successfully read list is authoritative for its own request kind.
/// A failed/malformed read must not erase an ask whose state is unknown.
pub(super) struct PendingRequestReadback {
    pub(super) requests: Vec<PendingRequest>,
    pub(super) permissions_read: bool,
    pub(super) forms_read: bool,
}

impl PendingRequestReadback {
    pub(super) fn reconcile(
        &self,
        pending: &mut HashMap<String, String>,
        owns: impl Fn(&str) -> bool,
    ) {
        let upstream: HashSet<&str> = self
            .requests
            .iter()
            .map(|request| request.id.as_str())
            .collect();
        pending.retain(|id, owner| {
            owns(owner)
                && (upstream.contains(id.as_str())
                    || !((self.permissions_read && self.forms_read)
                        || (self.permissions_read && id.starts_with("per"))
                        || (self.forms_read && id.starts_with("frm_"))))
        });
        for request in self
            .requests
            .iter()
            .filter(|request| owns(&request.session))
        {
            pending.insert(request.id.clone(), request.session.clone());
        }
    }
}

/// Pending permissions (`GET /api/permission/request`) and forms
/// (`GET /api/form`) in the run's location, as the frames that announce
/// them. The caller dedupes against ids it already saw.
pub(super) async fn pending_request_frames(
    server: &Server,
    dir: Option<&str>,
) -> PendingRequestReadback {
    let mut out = PendingRequestReadback {
        requests: Vec::new(),
        permissions_read: false,
        forms_read: false,
    };
    if let Ok(list) = server.get_json("/api/permission/request", dir).await
        && let Some(items) = unwrap_data(list).as_array()
    {
        out.permissions_read = true;
        for item in items {
            let (Some(id), Some(session)) = (
                item.get("id").and_then(Value::as_str),
                item.get("sessionID").and_then(Value::as_str),
            ) else {
                continue;
            };
            out.requests.push(PendingRequest {
                id: id.to_owned(),
                session: session.to_owned(),
                frame: json!({ "type": "permission.asked", "data": item, "replayed": true }),
            });
        }
    }
    if let Ok(list) = server.get_json("/api/form", dir).await
        && let Some(items) = unwrap_data(list).as_array()
    {
        out.forms_read = true;
        for item in items {
            let (Some(id), Some(session)) = (
                item.get("id").and_then(Value::as_str),
                item.get("sessionID").and_then(Value::as_str),
            ) else {
                continue;
            };
            out.requests.push(PendingRequest {
                id: id.to_owned(),
                session: session.to_owned(),
                frame: json!({ "type": "form.created", "data": { "form": item }, "replayed": true }),
            });
        }
    }
    out
}

/// The session record's answer for a turn whose terminal frame was lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionOutcome {
    /// This server still executes the session.
    Running,
    Succeeded,
    Failed,
    Interrupted,
    /// Not running, and no outcome recorded since the turn started: the
    /// execution was lost (server restart, crash).
    Lost,
    Unknown(String),
}

/// Read `GET /api/session/active` and the session record (`outcome`,
/// `time.idle`) for a turn that started at `since_ms`.
pub(super) async fn session_outcome(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    since_ms: i64,
) -> SessionOutcome {
    match server.session_running(session_id, dir).await {
        Ok(true) => return SessionOutcome::Running,
        Ok(false) => {}
        Err(error) => return SessionOutcome::Unknown(error.to_string()),
    }
    let info = match server.session_info(session_id, dir).await {
        Ok(info) => info,
        Err(error) => return SessionOutcome::Unknown(error.to_string()),
    };
    let idle = info.pointer("/time/idle").and_then(Value::as_i64);
    if idle.is_none_or(|idle| idle < since_ms) {
        return SessionOutcome::Lost;
    }
    match info.get("outcome").and_then(Value::as_str) {
        Some("succeeded") => SessionOutcome::Succeeded,
        Some("failed") => SessionOutcome::Failed,
        Some("interrupted") => SessionOutcome::Interrupted,
        _ => SessionOutcome::Lost,
    }
}
