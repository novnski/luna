//! Worker protocol 1.0 (CORE-00): UTF-8 JSON lines over the worker's stdio.
//! Every frame, in either direction, is bounded to [`FRAME_LIMIT`] before it
//! is decoded.
//!
//! Host to worker: `hello` (first), then `call`, `cancel`, `hostResult` and
//! `shutdown`. Worker to host: a `hello` reply, then `response`, `hostCall`,
//! `hostCancel` and `log`. Frames that ask the host for authority
//! (`hostCall`, `hostCancel`) refuse unknown fields, so a worker cannot add a
//! principal or target of its own. Informational frames ignore unknown fields
//! so a newer minor can add them; an unknown field never grants anything.

use std::io;

use luna_proto::{ExtensionContractVersion, ExtensionTrust};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::broker::BrokerError;

/// Largest control frame, excluding its newline.
pub(super) const FRAME_LIMIT: usize = 64 * 1024;
/// Longest frame id.
const ID_LIMIT: usize = 128;
/// Longest operation or method name.
pub(super) const NAME_LIMIT: usize = 128;
/// Longest retained log message.
pub(super) const LOG_LINE_LIMIT: usize = 2 * 1024;
const LEVEL_LIMIT: usize = 16;
const ERROR_CODE_LIMIT: usize = 64;
const ERROR_MESSAGE_LIMIT: usize = 2 * 1024;

pub(super) enum FrameError {
    /// More than [`FRAME_LIMIT`] bytes arrived without a newline.
    TooLarge,
    Io(io::Error),
}

/// Read one newline-terminated frame without ever buffering more than
/// [`FRAME_LIMIT`] bytes of it. `None` is end of input; a partial last line
/// is dropped with it.
pub(super) async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(FrameError::Io)?;
        if available.is_empty() {
            return Ok(None);
        }
        if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + end > FRAME_LIMIT {
                return Err(FrameError::TooLarge);
            }
            frame.extend_from_slice(&available[..end]);
            reader.consume(end + 1);
            return Ok(Some(frame));
        }
        let length = available.len();
        if frame.len() + length > FRAME_LIMIT {
            return Err(FrameError::TooLarge);
        }
        frame.extend_from_slice(available);
        reader.consume(length);
    }
}

/// Serialize one outbound frame and terminate it with a newline.
pub(super) fn encode<T: Serialize>(frame: &T) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(frame).map_err(|error| error.to_string())?;
    if bytes.len() > FRAME_LIMIT {
        return Err(format!(
            "the frame is larger than {} KiB",
            FRAME_LIMIT / 1024
        ));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Truncate to at most `max` bytes on a character boundary.
pub(super) fn bounded_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= ID_LIMIT && !id.chars().any(char::is_control)
}

// ---- Host to worker -------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HelloFrame<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a str,
    pub protocol: ExtensionContractVersion,
    pub api: ExtensionContractVersion,
    pub instance: HelloInstance<'a>,
    pub limits: HelloLimits,
    /// Operations the caller needs; the reply must advertise each.
    pub required: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portable: Option<HelloPortable>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HelloInstance<'a> {
    pub id: &'a str,
    pub extension_id: &'a str,
    pub version: &'a str,
    pub package_hash: &'a str,
    pub trust: ExtensionTrust,
    pub grants: &'a [String],
    pub target: HelloTarget<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum HelloTarget<'a> {
    None,
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: &'a str,
        /// Native workers only: the checkout they may run their own tools in.
        #[serde(skip_serializing_if = "Option::is_none")]
        root: Option<&'a str>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HelloLimits {
    pub frame_bytes: u32,
    pub max_concurrent_calls: u32,
    pub max_concurrent_host_calls: u32,
    pub host_call_timeout_ms: u64,
    pub memory_bytes: u64,
    pub cpu_seconds: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HelloPortable {
    pub guest_memory_bytes: u64,
    pub fuel_per_call: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CallFrame<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a str,
    pub method: &'a str,
    pub params: &'a Value,
    pub deadline_ms: u64,
}

#[derive(Serialize)]
pub(super) struct CancelFrame<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a str,
}

#[derive(Serialize)]
pub(super) struct HostResultFrame<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: &'a str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a BrokerError>,
}

#[derive(Serialize)]
pub(super) struct ShutdownFrame {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

// ---- Worker to host -------------------------------------------------------

/// The worker's reply to `hello`. Unknown fields are ignored (a newer minor
/// may add some); none of them grants anything.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkerHello {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub protocol: ExtensionContractVersion,
    pub extension_id: String,
    pub package_hash: String,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub portable: Option<PortableHello>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PortableHello {
    pub profile: ExtensionContractVersion,
    pub runtime: String,
    pub module_sha256: String,
    #[serde(default)]
    pub imports: Vec<String>,
}

pub(super) fn decode_hello(frame: &[u8]) -> Result<WorkerHello, String> {
    serde_json::from_slice(frame).map_err(|error| format!("the hello reply is invalid: {error}"))
}

/// A worker's error answer, bounded.
#[derive(Debug, Clone)]
pub(super) struct WorkerError {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize)]
struct RawWorkerError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

pub(super) enum Inbound {
    Response {
        id: String,
        outcome: Result<Value, WorkerError>,
    },
    HostCall {
        id: String,
        op: String,
        params: Value,
    },
    /// A `hostCall` with a readable id whose other fields the broker refuses
    /// (an unknown field, a missing `op`). Answered `badRequest`, not fatal.
    RefusedHostCall {
        id: String,
        reason: String,
    },
    HostCancel {
        id: String,
    },
    Log {
        level: Option<String>,
        message: String,
    },
}

#[derive(Deserialize)]
struct ResponseFrame {
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RawWorkerError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostCallFrame {
    #[allow(dead_code)]
    id: String,
    op: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostCancelFrame {
    id: String,
}

#[derive(Deserialize)]
struct LogFrame {
    #[serde(default)]
    level: Option<String>,
    message: String,
}

fn from_object<T: for<'de> Deserialize<'de>>(
    kind: &str,
    object: Map<String, Value>,
) -> Result<T, String> {
    serde_json::from_value(Value::Object(object))
        .map_err(|error| format!("a {kind} frame is invalid: {error}"))
}

/// Decode one frame after the handshake. `Err` is a protocol violation.
pub(super) fn decode(frame: &[u8]) -> Result<Inbound, String> {
    let value: Value =
        serde_json::from_slice(frame).map_err(|error| format!("a frame is not JSON: {error}"))?;
    let Value::Object(mut object) = value else {
        return Err("a frame must be a JSON object".into());
    };
    let kind = match object.remove("type") {
        Some(Value::String(kind)) => kind,
        _ => return Err("a frame needs a string `type`".into()),
    };
    match kind.as_str() {
        "response" => {
            let frame: ResponseFrame = from_object("response", object)?;
            if !valid_id(&frame.id) {
                return Err("a response needs an id of at most 128 bytes".into());
            }
            let outcome = match (frame.ok, frame.error) {
                (true, None) => Ok(frame.result.unwrap_or(Value::Null)),
                (false, Some(error)) => Err(WorkerError {
                    code: bounded_text(error.code.as_deref().unwrap_or("failed"), ERROR_CODE_LIMIT),
                    message: bounded_text(
                        error.message.as_deref().unwrap_or(""),
                        ERROR_MESSAGE_LIMIT,
                    ),
                }),
                (true, Some(_)) => return Err("a successful response carries an error".into()),
                (false, None) => return Err("a failed response needs an error".into()),
            };
            Ok(Inbound::Response {
                id: frame.id,
                outcome,
            })
        }
        "hostCall" => {
            let id = match object.get("id") {
                Some(Value::String(id)) if valid_id(id) => id.clone(),
                _ => return Err("a hostCall needs an id of at most 128 bytes".into()),
            };
            match serde_json::from_value::<HostCallFrame>(Value::Object(object)) {
                Ok(frame) if !frame.op.is_empty() && frame.op.len() <= NAME_LIMIT => {
                    Ok(Inbound::HostCall {
                        id,
                        op: frame.op,
                        params: frame.params,
                    })
                }
                Ok(_) => Ok(Inbound::RefusedHostCall {
                    id,
                    reason: "a hostCall needs an op of at most 128 bytes".into(),
                }),
                Err(error) => Ok(Inbound::RefusedHostCall {
                    id,
                    reason: format!("the hostCall frame is refused: {error}"),
                }),
            }
        }
        "hostCancel" => {
            let frame: HostCancelFrame = from_object("hostCancel", object)?;
            Ok(Inbound::HostCancel { id: frame.id })
        }
        "log" => {
            let frame: LogFrame = from_object("log", object)?;
            Ok(Inbound::Log {
                level: frame.level.map(|level| bounded_text(&level, LEVEL_LIMIT)),
                message: bounded_text(&frame.message, LOG_LINE_LIMIT),
            })
        }
        "hello" => Err("hello is only valid as the first frame".into()),
        other => Err(format!(
            "unknown frame type {:?}",
            bounded_text(other, NAME_LIMIT)
        )),
    }
}
