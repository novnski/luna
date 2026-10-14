//! Provider profile 2.0, the hosted harness profile (CORE-11).
//!
//! A provider package whose worker hosts a whole [`Harness`](crate::Harness)
//! adapter (OpenCode's first-party package hosts [`crate::OpencodeHarness`])
//! speaks this profile over worker protocol 1.0. Profile 1.0 (CORE-08) is a
//! thin normalized vocabulary for third-party CLIs; this profile carries
//! LUNA's own public wire types instead, so nothing the adapter emits is
//! re-modelled on the way to the engine:
//!
//! - every run event is the adapter's [`luna_proto::AgentEvent`] (stream
//!   errors keep their [`HarnessError`] kind), in the adapter's order;
//! - questions and approval cards are its [`luna_proto::UserInputQuestion`]s
//!   (MCP launch URLs, which never serialize, travel beside them);
//! - steering, LUNA tool calls, native child stop, titles, commands,
//!   extensions, MCP status, fork, history, rewind and compaction keep the
//!   adapter's own request and result values.
//!
//! Work is a **job** (`provider.job.start {job, op, params}`): the start call
//! only admits it, and everything it reports — run events, questions, tool
//! calls, steer acknowledgements, progress and its single terminal `done` —
//! arrives on one ordered event queue per worker that the host long-polls
//! (`provider.events {after, waitMs}`). Each queued item has the next `seq`
//! (1, 2, 3 …); `after` acknowledges every item at or below it, a repeated
//! `after` returns the same items again, and a gap is a conformance failure.
//! Values larger than a frame are split: the host uploads big parameters in
//! chunks (`provider.upload`, then `{"upload": id}` as the call's params) and
//! the worker splits big queue items into consecutive `fragment` items. No
//! item is dropped or truncated; a host that stops reading slows the adapter
//! down instead (its event stream waits).
//!
//! This module is the contract shared by the worker ([`worker`]) and the
//! engine's bridge: operation names, limits and the codecs for adapter types
//! that have no serde form.

use std::collections::BTreeMap;
use std::path::PathBuf;

use luna_proto::{
    PermissionMode, ProviderForkBoundary, ProviderForkRequest, ProviderForkResult,
    ProviderMcpAppResourceView, ProviderMcpStatus, ProviderMcpTool,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::mcp_control::{
    McpControlOp, McpControlReply, McpControlRequest, McpLaunchReadback, McpWriteOutcome,
    ProviderMcpServer, SessionMcpObservation, SessionMcpServer,
};
use crate::{
    CommandScope, ContextCompactRequest, HarnessError, LunaToolInvocationContext,
    NativeHistoryScope, NativeHistoryTurns, NativeRewindOutcome, NativeRewindRequest,
    NativeTaskStop, ProviderForkProgress, SteerAcceptance, SteerDelivery,
};

pub mod worker;

/// The profile a hosted worker describes; the bridge requires this major.
pub const PROFILE_MAJOR: u32 = 2;
pub const PROFILE_MINOR: u32 = 0;

/// `{state?}` → `{profile, providerId, available, unavailableReason?,
/// executable?, runtimeDelivery}`.
pub const DESCRIBE: &str = "provider.describe";
/// `{uploadId, index, count, data}` → `{received}`.
pub const UPLOAD: &str = "provider.upload";
/// `{after, waitMs}` → `{events: [{seq, type, ...}]}`.
pub const EVENTS: &str = "provider.events";
/// `{job, op, params}` → `{}`; the job reports on the event queue.
pub const JOB_START: &str = "provider.job.start";
/// `{job}` → `{cancelled}`: ends the job's task (a run's server with it).
pub const JOB_CANCEL: &str = "provider.job.cancel";
/// `{job, kind: input|tool|progress, id, ...}` → `{}`.
pub const JOB_REPLY: &str = "provider.job.reply";
/// `{job}` → `{stopping}`: the run's own interrupt, as a Stop does it.
pub const RUN_STOP: &str = "provider.run.stop";
/// `{job, steer, prompt, attachments, inputItems, messageId, delivery,
/// goal, acceptance}` → `{delivered}`; acceptance arrives as `steerAck`.
pub const RUN_STEER: &str = "provider.run.steer";
/// `{sessionId, childId}` → `{ok, stop | error}`.
pub const CHILD_STOP: &str = "provider.child.stop";

/// Every operation a hosted worker advertises; required at every start.
pub const OPERATIONS: [&str; 9] = [
    DESCRIBE, UPLOAD, EVENTS, JOB_START, JOB_CANCEL, JOB_REPLY, RUN_STOP, RUN_STEER, CHILD_STOP,
];

/// Job operations (`provider.job.start {op}`).
pub mod op {
    /// `{live}` → `{models}`.
    pub const MODELS: &str = "models";
    /// `{request, lunaTools, state}` → events, then `{state,
    /// runtimeDelivery}`.
    pub const RUN: &str = "run";
    /// `{request}` → `{events}`.
    pub const TITLE: &str = "title";
    /// `{scope}` → `{commands}`.
    pub const COMMANDS: &str = "commands";
    /// `{cwd, mode, refresh}` → `{view}`.
    pub const EXTENSIONS: &str = "extensions";
    /// `{request}` → `{reply}`.
    pub const MCP: &str = "mcp";
    /// `{request}` → acknowledged `progress`, then `{result}`.
    pub const FORK: &str = "fork";
    /// `{scope}` → `{turns}`.
    pub const HISTORY: &str = "history";
    /// `{request}` → acknowledged `progress`, then `{outcome}`.
    pub const REWIND: &str = "rewind";
    /// `{request}` → `{}`.
    pub const COMPACT: &str = "compact";

    pub const ALL: [&str; 10] = [
        MODELS, RUN, TITLE, COMMANDS, EXTENSIONS, MCP, FORK, HISTORY, REWIND, COMPACT,
    ];
}

/// Worker protocol 1.0's frame limit, newline excluded.
pub const FRAME_BYTES: usize = 64 * 1024;
/// Call parameters above this are uploaded first.
pub const INLINE_PARAMS_BYTES: usize = 40 * 1024;
/// Raw bytes of one upload chunk (at most twice that once JSON-escaped).
pub const UPLOAD_CHUNK_BYTES: usize = 16 * 1024;
/// Largest uploaded value and largest fragmented queue item.
pub const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Budget of one events page.
pub const PAGE_BYTES: usize = 40 * 1024;
/// A queue item larger than this is sent as fragments.
pub const FRAGMENT_THRESHOLD_BYTES: usize = 20 * 1024;
/// Raw bytes of one fragment (at most twice that once JSON-escaped).
pub const FRAGMENT_CHUNK_BYTES: usize = 8 * 1024;
pub const MAX_FRAGMENTS: usize = MAX_UPLOAD_BYTES / FRAGMENT_CHUNK_BYTES;

/// `text` in pieces of at most `max` bytes, cut on character boundaries.
pub fn split_text(text: &str, max: usize) -> Vec<&str> {
    let max = max.max(4);
    let mut parts = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(&text[start..end]);
        start = end;
    }
    parts
}

// ---------------------------------------------------------------------------
// Field helpers
// ---------------------------------------------------------------------------

fn text(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn optional_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn typed<T: DeserializeOwned>(value: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(value.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|error| format!("`{key}` is invalid: {error}"))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A [`HarnessError`] with its kind, so the bridge returns the same typed
/// boundary the adapter did (only `rejected` permits a recovery resend).
pub fn encode_error(error: &HarnessError) -> Value {
    let (kind, message) = match error {
        HarnessError::NotInstalled(message) => ("notInstalled", message.clone()),
        HarnessError::Protocol(message) => ("protocol", message.clone()),
        HarnessError::Tool(message) => ("tool", message.clone()),
        HarnessError::RejectedBeforeAcceptance(message) => ("rejected", message.clone()),
        HarnessError::Install(message) => ("install", message.clone()),
        HarnessError::Io(error) => ("io", error.to_string()),
    };
    json!({"kind": kind, "message": message})
}

pub fn decode_error(value: &Value) -> HarnessError {
    let message = optional_text(value, "message").unwrap_or_default();
    match value.get("kind").and_then(Value::as_str) {
        Some("notInstalled") => HarnessError::NotInstalled(message),
        Some("tool") => HarnessError::Tool(message),
        Some("rejected") => HarnessError::RejectedBeforeAcceptance(message),
        Some("install") => HarnessError::Install(message),
        Some("io") => HarnessError::Io(std::io::Error::other(message)),
        _ => HarnessError::Protocol(message),
    }
}

// ---------------------------------------------------------------------------
// Steering and child stop
// ---------------------------------------------------------------------------

pub fn encode_steer_delivery(delivery: SteerDelivery) -> &'static str {
    match delivery {
        SteerDelivery::AllowFollowUpTurn => "allowFollowUpTurn",
        SteerDelivery::RequireActiveTurn => "requireActiveTurn",
    }
}

pub fn decode_steer_delivery(value: &Value) -> Result<SteerDelivery, String> {
    match value.as_str() {
        Some("allowFollowUpTurn") => Ok(SteerDelivery::AllowFollowUpTurn),
        Some("requireActiveTurn") => Ok(SteerDelivery::RequireActiveTurn),
        _ => Err("`delivery` must be allowFollowUpTurn or requireActiveTurn".into()),
    }
}

/// `steerAck.acceptance` when the adapter dropped the acceptance unanswered.
pub fn steer_dropped() -> Value {
    json!({"status": "dropped"})
}

pub fn encode_steer_acceptance(acceptance: &SteerAcceptance) -> Value {
    match acceptance {
        SteerAcceptance::Accepted => json!({"status": "accepted"}),
        SteerAcceptance::Rejected(reason) => json!({"status": "rejected", "reason": reason}),
        SteerAcceptance::Unknown(reason) => json!({"status": "unknown", "reason": reason}),
    }
}

/// `None`: the adapter dropped the acceptance without answering, which the
/// caller reproduces by dropping its own sender.
pub fn decode_steer_acceptance(value: &Value) -> Result<Option<SteerAcceptance>, String> {
    let reason = optional_text(value, "reason").unwrap_or_default();
    match value.get("status").and_then(Value::as_str) {
        Some("accepted") => Ok(Some(SteerAcceptance::Accepted)),
        Some("rejected") => Ok(Some(SteerAcceptance::Rejected(reason))),
        Some("unknown") => Ok(Some(SteerAcceptance::Unknown(reason))),
        Some("dropped") => Ok(None),
        _ => Err("a steer acknowledgement needs a known status".into()),
    }
}

pub fn encode_task_stop(stop: &NativeTaskStop) -> Value {
    match stop {
        NativeTaskStop::Requested => json!({"status": "requested"}),
        NativeTaskStop::Rejected(reason) => json!({"status": "rejected", "reason": reason}),
        NativeTaskStop::Unknown(reason) => json!({"status": "unknown", "reason": reason}),
    }
}

pub fn decode_task_stop(value: &Value) -> Result<NativeTaskStop, String> {
    let reason = optional_text(value, "reason").unwrap_or_default();
    match value.get("status").and_then(Value::as_str) {
        Some("requested") => Ok(NativeTaskStop::Requested),
        Some("rejected") => Ok(NativeTaskStop::Rejected(reason)),
        Some("unknown") => Ok(NativeTaskStop::Unknown(reason)),
        _ => Err("a child stop answer needs a known status".into()),
    }
}

pub fn encode_tool_context(context: &LunaToolInvocationContext) -> Value {
    json!({
        "threadId": context.provider_thread_id(),
        "turnId": context.provider_turn_id(),
        "callId": context.provider_call_id(),
    })
}

pub fn decode_tool_context(value: &Value) -> Result<LunaToolInvocationContext, String> {
    Ok(LunaToolInvocationContext::new(
        &text(value, "threadId")?,
        &text(value, "turnId")?,
        &text(value, "callId")?,
    ))
}

// ---------------------------------------------------------------------------
// Commands, history, rewind, compaction and fork
// ---------------------------------------------------------------------------

pub fn encode_command_scope(scope: &CommandScope) -> Value {
    let cwd = scope
        .cwd
        .as_ref()
        .map(|cwd| cwd.to_string_lossy().into_owned());
    json!({
        "cwd": cwd,
        "permissionMode": scope.permission_mode,
        "authInstance": scope.auth_instance,
    })
}

pub fn decode_command_scope(value: &Value) -> Result<CommandScope, String> {
    Ok(CommandScope {
        cwd: optional_text(value, "cwd").map(PathBuf::from),
        permission_mode: typed::<PermissionMode>(value, "permissionMode")?,
        auth_instance: optional_text(value, "authInstance"),
    })
}

pub fn encode_history_scope(scope: &NativeHistoryScope) -> Value {
    json!({
        "sessionId": scope.session_id,
        "cwd": scope.cwd,
        "providerHome": scope.provider_home,
    })
}

pub fn decode_history_scope(value: &Value) -> Result<NativeHistoryScope, String> {
    Ok(NativeHistoryScope {
        session_id: text(value, "sessionId")?,
        cwd: text(value, "cwd")?,
        provider_home: optional_text(value, "providerHome"),
    })
}

pub fn encode_history_turns(turns: &NativeHistoryTurns) -> Value {
    json!({"turnStarts": turns.turn_starts, "compacted": turns.compacted})
}

pub fn decode_history_turns(value: &Value) -> Result<NativeHistoryTurns, String> {
    Ok(NativeHistoryTurns {
        turn_starts: typed::<Vec<String>>(value, "turnStarts")?,
        compacted: flag(value, "compacted"),
    })
}

pub fn encode_rewind_request(request: &NativeRewindRequest) -> Value {
    json!({
        "scope": encode_history_scope(&request.scope),
        "targetCheckpoint": request.target_checkpoint,
        "lastSeenCheckpoint": request.last_seen_checkpoint,
    })
}

pub fn decode_rewind_request(value: &Value) -> Result<NativeRewindRequest, String> {
    Ok(NativeRewindRequest {
        scope: decode_history_scope(value.get("scope").unwrap_or(&Value::Null))?,
        target_checkpoint: text(value, "targetCheckpoint")?,
        last_seen_checkpoint: text(value, "lastSeenCheckpoint")?,
    })
}

pub fn encode_rewind_outcome(outcome: &NativeRewindOutcome) -> Value {
    match outcome {
        NativeRewindOutcome::Rewound => json!({"status": "rewound"}),
        NativeRewindOutcome::Refused { reason, message } => {
            json!({"status": "refused", "reason": reason, "message": message})
        }
    }
}

pub fn decode_rewind_outcome(value: &Value) -> Result<NativeRewindOutcome, String> {
    match value.get("status").and_then(Value::as_str) {
        Some("rewound") => Ok(NativeRewindOutcome::Rewound),
        Some("refused") => Ok(NativeRewindOutcome::Refused {
            reason: optional_text(value, "reason").unwrap_or_default(),
            message: optional_text(value, "message").unwrap_or_default(),
        }),
        _ => Err("a rewind outcome needs a known status".into()),
    }
}

/// The rewind progress the adapter reports (`RequestWritten` only).
pub fn rewind_request_written() -> Value {
    json!({"kind": "requestWritten"})
}

pub fn encode_compact_request(request: &ContextCompactRequest) -> Value {
    json!({
        "sessionId": request.session_id,
        "cwd": request.cwd,
        "model": request.model,
    })
}

pub fn decode_compact_request(value: &Value) -> Result<ContextCompactRequest, String> {
    Ok(ContextCompactRequest {
        session_id: text(value, "sessionId")?,
        cwd: optional_text(value, "cwd").unwrap_or_default(),
        model: optional_text(value, "model"),
    })
}

pub fn encode_fork_request(request: &ProviderForkRequest) -> Value {
    json!({
        "sourceSessionId": request.source_session_id,
        "boundary": request.boundary,
        "cwd": request.cwd,
        "model": request.model,
        "targetName": request.target_name,
        "providerHome": request.provider_home,
        "forceFileAuth": request.force_file_auth,
    })
}

pub fn decode_fork_request(value: &Value) -> Result<ProviderForkRequest, String> {
    Ok(ProviderForkRequest {
        source_session_id: text(value, "sourceSessionId")?,
        boundary: typed::<ProviderForkBoundary>(value, "boundary")?,
        cwd: text(value, "cwd")?,
        model: optional_text(value, "model"),
        target_name: optional_text(value, "targetName"),
        provider_home: optional_text(value, "providerHome"),
        force_file_auth: flag(value, "forceFileAuth"),
    })
}

pub fn encode_fork_result(result: &ProviderForkResult) -> Value {
    json!({
        "providerSessionId": result.provider_session_id,
        "checkpointMap": result.checkpoint_map,
    })
}

pub fn decode_fork_result(value: &Value) -> Result<ProviderForkResult, String> {
    Ok(ProviderForkResult {
        provider_session_id: text(value, "providerSessionId")?,
        checkpoint_map: typed::<BTreeMap<String, String>>(value, "checkpointMap")?,
    })
}

pub fn encode_fork_progress(progress: &ProviderForkProgress) -> Value {
    match progress {
        ProviderForkProgress::MutationStarted => json!({"kind": "mutationStarted"}),
        ProviderForkProgress::Acknowledged {
            provider_session_id,
        } => json!({"kind": "acknowledged", "providerSessionId": provider_session_id}),
    }
}

pub fn decode_fork_progress(value: &Value) -> Result<ProviderForkProgress, String> {
    match value.get("kind").and_then(Value::as_str) {
        Some("mutationStarted") => Ok(ProviderForkProgress::MutationStarted),
        Some("acknowledged") => Ok(ProviderForkProgress::Acknowledged {
            provider_session_id: text(value, "providerSessionId")?,
        }),
        _ => Err("fork progress needs a known kind".into()),
    }
}

// ---------------------------------------------------------------------------
// MCP control and session observations
// ---------------------------------------------------------------------------

pub fn encode_mcp_request(request: &McpControlRequest) -> Value {
    let op = match &request.op {
        McpControlOp::Status { modes } => json!({"kind": "status", "modes": modes}),
        McpControlOp::SetEnabled { server, enabled } => {
            json!({"kind": "setEnabled", "server": server, "enabled": enabled})
        }
        McpControlOp::ReadAppResource { server, uri } => {
            json!({"kind": "readAppResource", "server": server, "uri": uri})
        }
    };
    let settings_source = request
        .settings_source
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let cwd = request.cwd.to_string_lossy().into_owned();
    json!({"settingsSource": settings_source, "cwd": cwd, "op": op})
}

pub fn decode_mcp_request(value: &Value) -> Result<McpControlRequest, String> {
    let op = value.get("op").ok_or("`op` is missing")?;
    let op = match op.get("kind").and_then(Value::as_str) {
        Some("status") => McpControlOp::Status {
            modes: typed::<Vec<PermissionMode>>(op, "modes")?,
        },
        Some("setEnabled") => McpControlOp::SetEnabled {
            server: text(op, "server")?,
            enabled: flag(op, "enabled"),
        },
        Some("readAppResource") => McpControlOp::ReadAppResource {
            server: text(op, "server")?,
            uri: text(op, "uri")?,
        },
        _ => return Err("an MCP control request needs a known op".into()),
    };
    Ok(McpControlRequest {
        settings_source: optional_text(value, "settingsSource").map(PathBuf::from),
        cwd: PathBuf::from(text(value, "cwd")?),
        op,
    })
}

fn encode_mcp_server(server: &ProviderMcpServer) -> Value {
    json!({
        "name": server.name,
        "status": server.status,
        "scope": server.scope,
        "transport": server.transport,
        "serverVersion": server.server_version,
        "error": server.error,
        "errorCode": server.error_code,
        "tools": server.tools,
        "toggleable": server.toggleable,
    })
}

fn decode_mcp_server(value: &Value) -> Result<ProviderMcpServer, String> {
    Ok(ProviderMcpServer {
        name: text(value, "name")?,
        status: typed::<ProviderMcpStatus>(value, "status")?,
        scope: optional_text(value, "scope"),
        transport: optional_text(value, "transport"),
        server_version: optional_text(value, "serverVersion"),
        error: optional_text(value, "error"),
        error_code: optional_text(value, "errorCode"),
        tools: typed::<Vec<ProviderMcpTool>>(value, "tools")?,
        toggleable: flag(value, "toggleable"),
    })
}

fn decode_mcp_servers(value: &Value) -> Result<Vec<ProviderMcpServer>, String> {
    value
        .as_array()
        .ok_or("MCP servers must be a list")?
        .iter()
        .map(decode_mcp_server)
        .collect()
}

fn encode_readback(readback: &McpLaunchReadback) -> Value {
    let servers: Vec<Value> = readback.servers.iter().map(encode_mcp_server).collect();
    json!({
        "modes": readback.modes,
        "restricted": readback.restricted,
        "settled": readback.settled,
        "servers": servers,
        "appReadSupported": readback.app_read_supported,
    })
}

fn decode_readback(value: &Value) -> Result<McpLaunchReadback, String> {
    Ok(McpLaunchReadback {
        modes: typed::<Vec<PermissionMode>>(value, "modes")?,
        restricted: flag(value, "restricted"),
        settled: flag(value, "settled"),
        servers: decode_mcp_servers(value.get("servers").unwrap_or(&Value::Null))?,
        app_read_supported: value.get("appReadSupported").and_then(Value::as_bool),
    })
}

pub fn encode_mcp_reply(reply: &McpControlReply) -> Value {
    match reply {
        McpControlReply::Status(readbacks) => {
            let readbacks: Vec<Value> = readbacks.iter().map(encode_readback).collect();
            json!({"kind": "status", "readbacks": readbacks})
        }
        McpControlReply::SetEnabled {
            outcome,
            readback,
            message,
        } => {
            let outcome = match outcome {
                McpWriteOutcome::Applied => "applied",
                McpWriteOutcome::Rejected => "rejected",
                McpWriteOutcome::Indeterminate => "indeterminate",
            };
            let readback: Option<Vec<Value>> = readback
                .as_ref()
                .map(|servers| servers.iter().map(encode_mcp_server).collect());
            json!({
                "kind": "setEnabled",
                "outcome": outcome,
                "readback": readback,
                "message": message,
            })
        }
        McpControlReply::AppResource(view) => json!({"kind": "appResource", "view": view}),
    }
}

pub fn decode_mcp_reply(value: &Value) -> Result<McpControlReply, String> {
    match value.get("kind").and_then(Value::as_str) {
        Some("status") => Ok(McpControlReply::Status(
            value
                .get("readbacks")
                .and_then(Value::as_array)
                .ok_or("`readbacks` must be a list")?
                .iter()
                .map(decode_readback)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Some("setEnabled") => Ok(McpControlReply::SetEnabled {
            outcome: match value.get("outcome").and_then(Value::as_str) {
                Some("applied") => McpWriteOutcome::Applied,
                Some("rejected") => McpWriteOutcome::Rejected,
                _ => McpWriteOutcome::Indeterminate,
            },
            readback: match value.get("readback") {
                None | Some(Value::Null) => None,
                Some(servers) => Some(decode_mcp_servers(servers)?),
            },
            message: optional_text(value, "message"),
        }),
        Some("appResource") => Ok(McpControlReply::AppResource(Box::new(typed::<
            ProviderMcpAppResourceView,
        >(
            value, "view"
        )?))),
        _ => Err("an MCP control reply needs a known kind".into()),
    }
}

pub fn encode_session_mcp(observation: &SessionMcpObservation) -> Value {
    let servers: Vec<Value> = observation
        .servers
        .iter()
        .map(|server| {
            json!({
                "name": server.name,
                "status": server.status,
                "source": server.source,
            })
        })
        .collect();
    json!({
        "sessionId": observation.session_id,
        "cwd": observation.cwd,
        "observedAtMs": observation.observed_at_ms,
        "servers": servers,
        "capabilitiesReported": observation.capabilities_reported,
        "appReadCapability": observation.app_read_capability,
        "appToolMetaCapability": observation.app_tool_meta_capability,
    })
}

pub fn decode_session_mcp(value: &Value) -> Result<SessionMcpObservation, String> {
    let servers = value
        .get("servers")
        .and_then(Value::as_array)
        .ok_or("`servers` must be a list")?
        .iter()
        .map(|server| {
            Ok(SessionMcpServer {
                name: text(server, "name")?,
                status: typed::<ProviderMcpStatus>(server, "status")?,
                source: optional_text(server, "source"),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(SessionMcpObservation {
        session_id: text(value, "sessionId")?,
        cwd: optional_text(value, "cwd").unwrap_or_default(),
        observed_at_ms: value
            .get("observedAtMs")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        servers,
        capabilities_reported: flag(value, "capabilitiesReported"),
        app_read_capability: flag(value, "appReadCapability"),
        app_tool_meta_capability: flag(value, "appToolMetaCapability"),
    })
}

/// A JSON object, or an empty one.
pub fn object_or_empty(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
