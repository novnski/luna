//! Claude CLI stream-json wire frames (stdout JSONL + stdin lines).
//!
//! Tolerant by construction: every field defaults, unknown frame types map to
//! [`Frame::Other`], so a newer CLI never breaks parsing — we only read the
//! fields the normalizer needs (spec: docs/research/harness.md).

use serde::Deserialize;
use serde_json::{Value, json};

use super::events::{self, NativeFrame, ResultEnvelope, TurnStamp};

const MAX_INIT_OBSERVATIONS: usize = 128;
const MAX_INIT_NAME_BYTES: usize = 256;

/// One parsed stdout line.
#[derive(Debug)]
pub(crate) enum Frame {
    System(SystemFrame),
    StreamEvent(StreamEventFrame),
    Assistant(MessageFrame),
    User(MessageFrame),
    RateLimit(RateLimitFrame),
    Result(ResultFrame),
    ControlRequest(ControlRequestFrame),
    /// CC-02: every other frame type, classified (decoded lifecycle, known
    /// progress, documented-but-unprojected, unknown) for evidence retention.
    Native(NativeFrame),
    /// control_response / control_cancel_request (transport-owned).
    Other,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SystemFrame {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub session_id: String,
    /// `init`/`status`: the native permission mode Claude actually runs.
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: Option<String>,
    /// Read-only wire observation. This is not a configured inventory, health
    /// check, authorization, or proof of selected-session attachment.
    #[serde(skip)]
    #[allow(dead_code)] // Downstream inventory mapping needs owner evidence.
    pub integration_observation: Option<InitIntegrationObservation>,
    /// `task_notification` (a background subagent settling): the spawning
    /// Agent tool's id — the only TAGGED terminal signal the 2.1.x wire has
    /// for a background subagent (its frames otherwise just stop).
    #[serde(default, alias = "toolUseId")]
    pub tool_use_id: Option<String>,
    /// `task_notification` terminal status (`completed`/`failed`/`killed`…).
    #[serde(default)]
    pub status: Option<String>,
    /// `task_started`: the agent/task id (`SendMessage`'s `to:` address) —
    /// with `tool_use_id`, the agentId→spawn mapping steers need.
    #[serde(default, alias = "taskId")]
    pub task_id: Option<String>,
    /// `task_started`: present only for AGENT tasks (a subagent spawning),
    /// absent on subagent-owned background shell tasks.
    #[serde(default)]
    pub subagent_type: Option<String>,
    /// CC-02 classification of this system frame (task/hook/compaction
    /// lifecycle, init readback, progress, unprojected or unknown).
    #[serde(skip)]
    pub native: Option<Box<NativeFrame>>,
    /// `init`: raw MCP rows and capability words, decoded only by `mcp.rs`.
    #[serde(default)]
    pub mcp_servers: Option<Box<Value>>,
    #[serde(default)]
    pub capabilities: Option<Box<Value>>,
    /// Allowlisted `task_*` fields (type, usage, patch, summary…), read
    /// leniently by `native_tasks`; nothing else is retained.
    #[serde(flatten)]
    pub native_task_fields: super::native_tasks::TaskFields,
    /// `peer_message_hold` fields (state/lane/from/cause/outcome…).
    #[serde(flatten)]
    pub peer_hold: Box<super::peer::PeerHoldFields>,
    /// Workflow/teammate task fields (tolerant raw values; see coordination).
    #[serde(flatten)]
    pub task_fields: super::coordination::TaskFrameFields,
    /// Hook lifecycle / `commands_changed` (decoded by `hooks.rs`).
    #[serde(skip)]
    pub extension: Option<Box<super::hooks::ExtensionFrame>>,
}

/// Allowlisted names and status words from one `system:init` frame. Unknown
/// fields and credential-bearing payloads are never retained here.
#[derive(Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // Deliberately internal until a source owner can be mapped.
pub(crate) struct InitIntegrationObservation {
    pub mcp_servers: Vec<ObservedMcpServer>,
    pub skill_names: Vec<String>,
    pub tool_names: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ObservedMcpServer {
    pub name: String,
    pub status: ObservedMcpStatus,
}

/// Only status values seen in the checked-in Claude init fixture. An unknown
/// future status omits that observation; it never becomes a health claim.
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ObservedMcpStatus {
    Connected,
    NeedsAuth,
}

fn observed_name(value: &Value) -> Option<String> {
    let name = value.as_str()?;
    if name.trim() != name
        || !name.bytes().any(|byte| byte.is_ascii_alphanumeric())
        || name.len() > MAX_INIT_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b' '))
    {
        return None;
    }
    Some(name.to_owned())
}

fn observed_names(value: Option<&Value>) -> Vec<String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    if items.len() > MAX_INIT_OBSERVATIONS {
        return Vec::new();
    }
    items.iter().filter_map(observed_name).collect()
}

fn observed_mcp_servers(value: Option<&Value>) -> Vec<ObservedMcpServer> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    if items.len() > MAX_INIT_OBSERVATIONS {
        return Vec::new();
    }
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let name = observed_name(object.get("name")?)?;
            let status = match object.get("status")?.as_str()? {
                "connected" => ObservedMcpStatus::Connected,
                "needs-auth" => ObservedMcpStatus::NeedsAuth,
                _ => return None,
            };
            Some(ObservedMcpServer { name, status })
        })
        .collect()
}

fn init_integration_observation(value: &Value) -> InitIntegrationObservation {
    InitIntegrationObservation {
        mcp_servers: observed_mcp_servers(value.get("mcp_servers")),
        skill_names: observed_names(value.get("skills")),
        tool_names: observed_names(value.get("tools")),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamEventFrame {
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub event: StreamEventBody,
    /// CC-02 turn identity (`user_message_uuid(s)`, origin, native uuid).
    #[serde(skip)]
    pub stamp: TurnStamp,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamEventBody {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub delta: Delta,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
}

/// An `assistant` or `user` frame (an Anthropic API message envelope).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessageFrame {
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub message: MessageBody,
    /// Terse assistant-level error code (`rate_limit`, `billing_error`, …).
    #[serde(default)]
    pub error: Option<String>,
    /// CC-02 turn identity (`user_message_uuid(s)`, origin, native uuid).
    #[serde(skip)]
    pub stamp: TurnStamp,
    /// User frames: the tool's full native Output object (or the MCP
    /// `{content, _meta?, structuredContent?, resourceLinks?}` spread).
    /// Projected by [`super::tool_results`].
    #[serde(default)]
    pub tool_use_result: Option<Value>,
    /// User frames: `[{id, non_execution_kind?, user_feedback?, remedy?}]`.
    #[serde(default)]
    pub tool_result_meta: Option<Value>,
    /// User-replay provenance (`--replay-user-messages`): human, peer, …
    #[serde(default)]
    pub origin: Option<Value>,
    #[serde(default)]
    pub uuid: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessageBody {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    /// Either a plain string or an array of content blocks.
    #[serde(default)]
    pub content: Value,
}

impl MessageBody {
    pub fn blocks(&self) -> impl Iterator<Item = ContentBlock> + '_ {
        self.content
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ContentBlock {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub is_error: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitFrame {
    #[serde(default)]
    pub rate_limit_info: RateLimitInfo,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitInfo {
    #[serde(default)]
    pub status: String,
    #[serde(rename = "rateLimitType", default)]
    pub rate_limit_type: Option<String>,
    /// Epoch seconds when the limiting window resets (CC-06).
    #[serde(rename = "resetsAt", default)]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ResultFrame {
    #[serde(default, rename = "modelUsage")]
    pub model_usage: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub errors: Vec<Value>,
    #[serde(default)]
    pub usage: UsageBody,
    #[serde(default)]
    pub session_id: Option<String>,
    /// `--json-schema` runs: the validated value, distinct from `result`.
    #[serde(default)]
    pub structured_output: Option<Value>,
    /// CC-02: the complete, per-field tolerant decode this frame came from.
    #[serde(skip)]
    pub envelope: Box<ResultEnvelope>,
}

impl ResultFrame {
    /// A result frame always yields an outcome: fields of an unexpected type
    /// are dropped (and named in the envelope), never fatal to the frame.
    fn from_envelope(envelope: ResultEnvelope) -> Self {
        Self {
            model_usage: envelope.model_usage.clone(),
            subtype: envelope.subtype.clone(),
            result: envelope.result.clone(),
            errors: envelope.errors.iter().cloned().map(Value::String).collect(),
            usage: UsageBody {
                input_tokens: envelope.usage.input_tokens,
                output_tokens: envelope.usage.output_tokens,
            },
            session_id: envelope.session_id.clone(),
            structured_output: envelope.structured_output.clone(),
            envelope: Box::new(envelope),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct UsageBody {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// A CLI→client control request. The request body stays raw: the CC-03
/// dispatcher (`controls.rs`) reads every subtype's own fields.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlRequestFrame {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub request: Value,
}

/// Parse one stdout JSONL line. `Err` = not JSON; unknown types = `Other`.
pub(crate) fn parse_frame(line: &str) -> Result<Frame, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let frame = match kind.as_str() {
        "system" => {
            let subtype = value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let observation = (subtype == "init").then(|| init_integration_observation(&value));
            let native = events::decode_system(&subtype, &value, line);
            let extension = super::hooks::parse_extension_frame(&value);
            let mut system: SystemFrame = serde_json::from_value(value)?;
            system.integration_observation = observation;
            system.native = Some(Box::new(native));
            system.extension = extension.map(Box::new);
            Frame::System(system)
        }
        "stream_event" => {
            let stamp = TurnStamp::decode(&value);
            let mut frame: StreamEventFrame = serde_json::from_value(value)?;
            frame.stamp = stamp;
            Frame::StreamEvent(frame)
        }
        "assistant" | "user" => {
            let stamp = TurnStamp::decode(&value);
            let mut frame: MessageFrame = serde_json::from_value(value)?;
            frame.stamp = stamp;
            if kind == "assistant" {
                Frame::Assistant(frame)
            } else {
                Frame::User(frame)
            }
        }
        "rate_limit_event" => Frame::RateLimit(serde_json::from_value(value)?),
        "result" => Frame::Result(ResultFrame::from_envelope(ResultEnvelope::decode(&value))),
        "control_request" => Frame::ControlRequest(serde_json::from_value(value)?),
        "control_response" | "control_cancel_request" => Frame::Other,
        other => Frame::Native(events::decode_top_level(other, &value, line)),
    };
    Ok(frame)
}

/// A stdin user turn: `{"type":"user","message":{...},"parent_tool_use_id":null}`.
/// Steering = another such line mid-run (consumed at a step boundary).
pub(crate) fn user_message_line(text: &str) -> String {
    let mut line = json!({
        "type": "user",
        "message": { "role": "user", "content": text },
        "parent_tool_use_id": null,
    });
    // LUNA's own peer envelope declares the sender class (peer origin only).
    if let Some(origin) = super::peer::host_declared_origin(text) {
        line["origin"] = origin;
    }
    line.to_string()
}

/// Success reply to a CLI control request (`can_use_tool` allow/deny payloads).
pub(crate) fn control_response_line(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_and_unknown_frames() {
        let init = r#"{"type":"system","subtype":"init","model":"m","tools":["Bash"],"cwd":"/x","session_id":"s1"}"#;
        match parse_frame(init).expect("parses") {
            Frame::System(f) => {
                assert_eq!(f.subtype, "init");
                assert_eq!(f.session_id, "s1");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        // CC-02: unknown types are classified for retention, not dropped.
        assert!(matches!(
            parse_frame(r#"{"type":"mystery_frame"}"#).expect("parses"),
            Frame::Native(_)
        ));
        assert!(parse_frame("not json").is_err());
    }

    #[test]
    fn init_observation_keeps_only_allowlisted_names_and_statuses() {
        let frame = json!({
            "type": "system",
            "subtype": "init",
            "model": "synthetic-model",
            "cwd": "/synthetic-project",
            "session_id": "synthetic-session",
            "tools": ["Bash", "mcp__demo__read", "bad/tool"],
            "skills": ["review", "code-review", "bad/skill", "  ", 42],
            "mcp_servers": [
                {"name": "Local Demo", "status": "connected", "auth": {"token": "SECRET_SENTINEL"}},
                {"name": "Second", "status": "needs-auth", "url": "SECRET_URL_SENTINEL"},
                {"name": "Future", "status": "warming-up"},
                {"name": "bad/server", "status": "connected"},
                {"name": " leading", "status": "connected"}
            ],
            "credentials": {"value": "SECRET_OTHER_SENTINEL"}
        });
        let Frame::System(system) = parse_frame(&frame.to_string()).unwrap() else {
            panic!("expected system frame");
        };
        // The established SessionStarted inputs remain intact.
        assert_eq!(system.model, "synthetic-model");
        assert_eq!(system.cwd, "/synthetic-project");
        assert_eq!(system.session_id, "synthetic-session");
        assert_eq!(system.tools, ["Bash", "mcp__demo__read", "bad/tool"]);
        let frame_debug = format!("{system:?}");
        assert!(!frame_debug.contains("SECRET_SENTINEL"));
        assert!(!frame_debug.contains("SECRET_URL_SENTINEL"));
        assert!(!frame_debug.contains("SECRET_OTHER_SENTINEL"));

        let observed = system.integration_observation.unwrap();
        assert_eq!(observed.tool_names, ["Bash", "mcp__demo__read"]);
        assert_eq!(observed.skill_names, ["review", "code-review"]);
        assert_eq!(
            observed.mcp_servers,
            [
                ObservedMcpServer {
                    name: "Local Demo".into(),
                    status: ObservedMcpStatus::Connected,
                },
                ObservedMcpServer {
                    name: "Second".into(),
                    status: ObservedMcpStatus::NeedsAuth,
                },
            ]
        );
        let public_debug = format!("{observed:?}");
        assert!(!public_debug.contains("SECRET_SENTINEL"));
        assert!(!public_debug.contains("SECRET_URL_SENTINEL"));
        assert!(!public_debug.contains("SECRET_OTHER_SENTINEL"));
    }

    #[test]
    fn missing_unknown_and_malformed_observation_fields_do_not_break_init() {
        let Frame::System(empty) =
            parse_frame(r#"{"type":"system","subtype":"init","model":"m","session_id":"s"}"#)
                .unwrap()
        else {
            panic!("expected system frame");
        };
        assert_eq!(empty.model, "m");
        assert_eq!(
            empty.integration_observation,
            Some(InitIntegrationObservation::default())
        );

        let malformed = json!({
            "type": "system", "subtype": "init", "session_id": "s",
            "tools": ["Bash"], "skills": {"name": "not-an-array"},
            "mcp_servers": [null, 12, {"name": "Missing status"},
                {"name": "Unknown status", "status": "future-status"}]
        });
        let Frame::System(system) = parse_frame(&malformed.to_string()).unwrap() else {
            panic!("expected system frame");
        };
        let observed = system.integration_observation.unwrap();
        assert_eq!(observed.tool_names, ["Bash"]);
        assert!(observed.skill_names.is_empty());
        assert!(observed.mcp_servers.is_empty());
        assert_eq!(system.tools, ["Bash"]);

        let Frame::System(non_init) = parse_frame(
            r#"{"type":"system","subtype":"task_notification","mcp_servers":[{"name":"Other","status":"connected"}]}"#,
        )
        .unwrap() else {
            panic!("expected system frame");
        };
        assert!(non_init.integration_observation.is_none());
        // The established parser rejects a malformed `tools` event field.
        assert!(parse_frame(r#"{"type":"system","subtype":"init","tools":{}}"#).is_err());
    }

    #[test]
    fn oversized_observation_fields_are_ignored_without_truncation_claims() {
        let too_many = (0..=MAX_INIT_OBSERVATIONS)
            .map(|index| format!("item{index}"))
            .collect::<Vec<_>>();
        let too_many_servers = too_many
            .iter()
            .map(|name| json!({"name": name, "status": "connected"}))
            .collect::<Vec<_>>();
        let frame = json!({
            "type": "system", "subtype": "init", "session_id": "s",
            "tools": too_many, "skills": too_many,
            "mcp_servers": too_many_servers
        });
        let Frame::System(system) = parse_frame(&frame.to_string()).unwrap() else {
            panic!("expected system frame");
        };
        assert_eq!(system.tools.len(), MAX_INIT_OBSERVATIONS + 1);
        assert_eq!(
            system.integration_observation,
            Some(InitIntegrationObservation::default())
        );

        let too_long = "x".repeat(MAX_INIT_NAME_BYTES + 1);
        let frame = json!({
            "type": "system", "subtype": "init", "tools": [too_long],
            "skills": [too_long],
            "mcp_servers": [{"name": too_long, "status": "connected"}]
        });
        let Frame::System(system) = parse_frame(&frame.to_string()).unwrap() else {
            panic!("expected system frame");
        };
        assert_eq!(system.tools.len(), 1);
        assert_eq!(
            system.integration_observation,
            Some(InitIntegrationObservation::default())
        );
    }

    #[test]
    fn user_line_shape_matches_protocol() {
        let line = user_message_line("hi");
        let v: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"], "hi");
        assert!(v["parent_tool_use_id"].is_null());
    }
}
