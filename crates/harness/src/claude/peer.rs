//! Native Claude cross-session ("peer") messaging, CLI 2.1.286 contract.
//!
//! Three concerns, all keyed to wire shapes captured from the installed CLI
//! (`$B/evidence/cc09-children/probes/`):
//!
//! * **LUNA child reports.** The engine hands a Claude parent its child chat's
//!   report as a canonical, host-injected `<cross-session-message>` envelope.
//!   Claude classifies a stdin line by that content as `origin.kind = "peer"`,
//!   and the content wins over any stamped origin. It then frames the line for
//!   the model as another session's message, explicitly "never your user's
//!   approval". Unmarked text would instead read as a human turn. The host
//!   declares the sender's permission class through the stdin frame's own
//!   `origin.fromMode`; the envelope attribute alone is not honored.
//! * **Native peer arrivals.** With `--replay-user-messages`, a message another
//!   session sent over the local socket echoes back as a `user` frame whose
//!   `origin` carries sender-asserted `from`/`name`, optional kernel-verified
//!   `verifiedPeerPid` and the decoded `body`. It becomes a provider-neutral
//!   [`AgentEvent::PeerMessage`] observation, never a user turn.
//! * **Receive-side holds.** `system/peer_message_hold` reports a message the
//!   CLI parked (`held`) or resolved (`released`/`dropped`). A held stdin
//!   injection with no open turn would otherwise leave the LUNA turn waiting
//!   forever, so it ends with a visible error. Nothing is resent.

use luna_proto::{AgentEvent, DoneStatus, PermissionMode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::wire::{Frame, MessageFrame, SystemFrame};

const TAG: &str = "cross-session-message";
/// Sender address namespace for LUNA child reports. It is not a native
/// `uds:`/`bridge:` address, so Claude's SendMessage cannot reach it (the CLI
/// says so to the model); replies go through LUNA.
pub const LUNA_CHILD_PEER_PREFIX: &str = "luna-child:";
const MAX_NAME_CHARS: usize = 64;
const MAX_LABEL_CHARS: usize = 300;
const MAX_BODY_CHARS: usize = 4_000;

/// Claude's receive-side permission class for a LUNA mode. Only
/// `bypassPermissions` is "bypass" (verified in the 2.1.286 policy: `auto`
/// and `acceptEdits` are "prompting").
pub fn permission_class(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Full => "bypass",
        PermissionMode::Ask | PermissionMode::Auto => "prompting",
    }
}

/// Claude accepts a peer message without a human review hold only when the
/// declared sender class equals the receiver's own class. A headless host has
/// no approval surface, so a parity hold always ends `expired`/`discarded`.
pub fn delivers_without_hold(sender: PermissionMode, receiver: PermissionMode) -> bool {
    permission_class(sender) == permission_class(receiver)
}

/// `from-session` must match the CLI's `^[A-Za-z0-9_-]{1,80}$` or it is dropped.
fn valid_session_token(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A display name that is a fixed point of the CLI's own normalization (no
/// quotes/angle brackets, no control/format/separator code points, trimmed,
/// at most 64 code points), so the envelope survives its canonical round trip.
fn envelope_name(title: &str) -> Option<String> {
    let kept: String = title
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == ' ' || "-_.,:;!?()[]/&+'#@%*=~".contains(*c))
        .collect();
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = collapsed.chars().take(MAX_NAME_CHARS).collect();
    let name = name.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Escape an embedded `</cross-session-message` closer the way the CLI's own
/// envelope writer does (`<` → `<\\`), so a child's text can never terminate
/// the envelope early and the canonical round trip holds. Other markup is
/// left untouched.
fn escape_body(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let mut out = String::with_capacity(body.len() + 8);
    let mut last = 0;
    for (index, _) in body.match_indices('<') {
        let after = &lower[index + 1..];
        let closes = !after.starts_with('\\')
            && after
                .trim_start()
                .strip_prefix('/')
                .is_some_and(|tail| tail.trim_start().starts_with(TAG));
        if closes {
            out.push_str(&body[last..=index]);
            out.push('\\');
            last = index + 1;
        }
    }
    out.push_str(&body[last..]);
    out
}

/// The canonical host-injected envelope for one LUNA child report delivered to
/// a Claude parent. `child_mode` is the child's effective permission mode, so
/// the declared class is truthful. Returns `None` when the child id cannot be
/// a canonical session token (the caller then keeps its plain text).
pub fn luna_child_report_envelope(
    child_task_id: &str,
    child_title: Option<&str>,
    child_mode: PermissionMode,
    body: &str,
) -> Option<String> {
    if !valid_session_token(child_task_id) {
        return None;
    }
    let mut attrs =
        format!(r#"from="{LUNA_CHILD_PEER_PREFIX}{child_task_id}" from-session="{child_task_id}""#);
    if let Some(name) = child_title.and_then(envelope_name) {
        attrs.push_str(&format!(r#" from-name="{name}""#));
    }
    attrs.push_str(&format!(r#" from-mode="{}""#, permission_class(child_mode)));
    Some(format!("<{TAG} {attrs}>\n{}\n</{TAG}>", escape_body(body)))
}

fn attribute<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!(r#" {name}=""#);
    let start = head.find(&marker)? + marker.len();
    let end = head[start..].find('"')?;
    Some(&head[start..start + end])
}

/// True when `text` is a LUNA-formed child report envelope (the only peer
/// envelope LUNA itself writes). Ultrathink and similar prompt rewrites must
/// not touch it, because a prefix would break Claude's content classification.
pub fn is_luna_peer_envelope(text: &str) -> bool {
    host_declared_origin(text).is_some()
}

/// The stdin `origin` LUNA declares for its own envelope. Claude honors the
/// sender class (`fromMode`) only from the injecting host on local stdin. Only
/// LUNA's namespace is lifted, and only with a matching `from-session`.
pub(crate) fn host_declared_origin(text: &str) -> Option<Value> {
    let rest = text.strip_prefix(&format!(r#"<{TAG} from=""#))?;
    let head = &text[..text.find('>')?];
    let from_end = rest.find('"')?;
    let from = &rest[..from_end];
    let id = from.strip_prefix(LUNA_CHILD_PEER_PREFIX)?;
    if !valid_session_token(id) || attribute(head, "from-session") != Some(id) {
        return None;
    }
    if !text.ends_with(&format!("\n</{TAG}>")) {
        return None;
    }
    let mut origin = json!({ "kind": "peer", "from": from });
    if let Some(mode) =
        attribute(head, "from-mode").filter(|mode| matches!(*mode, "bypass" | "prompting"))
    {
        origin["fromMode"] = json!(mode);
    }
    Some(origin)
}

/// Optional `system/peer_message_hold` fields, flattened into [`SystemFrame`].
/// Every field is a raw value so a future shape can never fail frame parsing.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct PeerHoldFields {
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    lane: Option<Value>,
    #[serde(default)]
    from: Option<Value>,
    #[serde(default)]
    from_name: Option<Value>,
    #[serde(default)]
    cause: Option<Value>,
    #[serde(default)]
    outcome: Option<Value>,
    #[serde(default)]
    message_uuid: Option<Value>,
    /// The frame's own id: a stable dedupe key when the host supplied no
    /// message uuid (LUNA's stdin turns carry none today).
    #[serde(default)]
    uuid: Option<Value>,
}

fn text_field(value: &Option<Value>, limit: usize) -> Option<String> {
    let text = value.as_ref()?.as_str()?;
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(limit)
        .collect();
    let clean = clean.trim().to_owned();
    (!clean.is_empty()).then_some(clean)
}

fn value_text(value: Option<&Value>, limit: usize) -> Option<String> {
    text_field(&value.cloned(), limit)
}

/// Per-run peer bookkeeping inside the Claude normalizer.
#[derive(Debug, Default)]
pub(crate) struct PeerState {
    /// A root turn is in progress (init/assistant/stream seen since the last
    /// result). A stdin hold outside a turn means our injected line never
    /// started one.
    turn_open: bool,
}

impl PeerState {
    pub(crate) fn observe(&mut self, frame: &Frame) {
        match frame {
            Frame::Result(_) => self.turn_open = false,
            Frame::StreamEvent(f) if f.parent_tool_use_id.is_none() => self.turn_open = true,
            Frame::Assistant(f) if f.parent_tool_use_id.is_none() => self.turn_open = true,
            Frame::System(f) if f.subtype == "init" => self.turn_open = true,
            _ => {}
        }
    }

    /// `system/peer_message_hold` → observation, plus a terminal error when our
    /// own injected line was parked before any turn started.
    pub(crate) fn hold_events(&mut self, frame: &SystemFrame) -> Vec<AgentEvent> {
        let hold = &frame.peer_hold;
        let state = text_field(&hold.state, 32).unwrap_or_else(|| "unknown".into());
        let lane = text_field(&hold.lane, 32).unwrap_or_else(|| "unknown".into());
        let name = text_field(&hold.from_name, MAX_NAME_CHARS + 1);
        let cause = text_field(&hold.cause, 64);
        let mut out = vec![AgentEvent::PeerMessage {
            state: state.clone(),
            lane: lane.clone(),
            from: text_field(&hold.from, MAX_LABEL_CHARS).unwrap_or_default(),
            name: name.clone(),
            from_session: None,
            verified_peer_pid: None,
            body: None,
            cause: cause.clone(),
            outcome: text_field(&hold.outcome, 64),
            message_uuid: text_field(&hold.message_uuid, 128)
                .or_else(|| text_field(&hold.uuid, 128)),
            host_injected: lane == "stdin",
        }];
        if lane == "stdin" && state == "held" && !self.turn_open {
            let who = name.unwrap_or_else(|| "a child chat".into());
            let why = cause.unwrap_or_else(|| "receive policy".into());
            let message = format!(
                "Claude held the report from {who} ({why}) instead of delivering it; it has not reached the model. LUNA will not resend it. Review the child chat and continue manually."
            );
            out.push(AgentEvent::Error {
                message: message.clone(),
            });
            out.push(AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some(message),
                session_id: None,
            });
        }
        out
    }

    /// A root `user` replay whose origin is a native peer that LUNA did not
    /// inject. `None` lets the ordinary path run (LUNA's own echoes, human or
    /// unstamped lines, other origin kinds produce no events there).
    pub(crate) fn arrival_events(&mut self, frame: &MessageFrame) -> Option<Vec<AgentEvent>> {
        if frame.parent_tool_use_id.is_some() {
            return None;
        }
        let origin = frame.origin.as_ref()?.as_object()?;
        if origin.get("kind").and_then(Value::as_str) != Some("peer")
            || origin.get("hostInjected").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let from = value_text(origin.get("from"), MAX_LABEL_CHARS).unwrap_or_default();
        let verified_peer_pid = origin
            .get("verifiedPeerPid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok());
        let lane = if verified_peer_pid.is_some() || from.starts_with("uds:") {
            "socket"
        } else if from.starts_with("bridge:") {
            "bridge"
        } else {
            "unknown"
        };
        let body = value_text(origin.get("body"), MAX_BODY_CHARS).or_else(|| {
            let text = match &frame.message.content {
                Value::String(text) => text.clone(),
                _ => frame
                    .message
                    .blocks()
                    .filter(|block| block.kind == "text")
                    .map(|block| block.text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            value_text(Some(&Value::String(text)), MAX_BODY_CHARS)
        });
        Some(vec![AgentEvent::PeerMessage {
            state: "delivered".into(),
            lane: lane.into(),
            from,
            name: value_text(origin.get("name"), MAX_NAME_CHARS + 1),
            from_session: value_text(origin.get("fromSession"), 128),
            verified_peer_pid,
            body,
            cause: None,
            outcome: None,
            message_uuid: frame
                .uuid
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_owned),
            host_injected: false,
        }])
    }
}
