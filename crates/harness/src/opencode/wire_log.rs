//! Opt-in QA wire log (OC-01/OC-02 evidence).
//!
//! `LUNA_OPENCODE_WIRE_LOG=<dir>` appends one JSONL file per LUNA process,
//! `<dir>/opencode-wire-<pid>.jsonl`, with:
//! - `sse`: every `/api/event` frame with its receipt time, the bus
//!   connection generation, the event id/type/session and `durable.seq`;
//! - `sse.connect` / `sse.end`: bus (re)connections and stream ends;
//! - `http`: every request LUNA makes — method, path, status, elapsed time,
//!   and the request/response bodies as a SHA-256 plus a redacted copy.
//!
//! Headers are never recorded (so neither is `Authorization`), the server
//! password is registered as a secret and replaced wherever it appears, and
//! credential-looking keys are redacted. Bodies are clipped at
//! [`BODY_LIMIT`]; the hash always covers the full body. Disabled (the
//! default), every hook is a cheap no-op.

use std::io::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::*;

pub(super) const WIRE_LOG_ENV: &str = "LUNA_OPENCODE_WIRE_LOG";

/// Redacted body bytes kept per record (the hash covers everything).
const BODY_LIMIT: usize = 64 * 1024;

/// Keys whose values never reach the log, matched case-insensitively.
const SECRET_KEYS: &[&str] = &[
    "password",
    "authorization",
    "token",
    "accesstoken",
    "refreshtoken",
    "apikey",
    "api_key",
    "secret",
    "cookie",
    "credential",
    "credentials",
];

pub(super) struct WireLog {
    file: Mutex<std::fs::File>,
    secrets: Mutex<Vec<String>>,
}

/// The process's wire log, when `LUNA_OPENCODE_WIRE_LOG` names a writable
/// directory.
pub(super) fn wire_log() -> Option<&'static WireLog> {
    static LOG: OnceLock<Option<WireLog>> = OnceLock::new();
    LOG.get_or_init(|| {
        let dir = std::env::var_os(WIRE_LOG_ENV).filter(|dir| !dir.is_empty())?;
        let dir = PathBuf::from(dir);
        let path = dir.join(format!("opencode-wire-{}.jsonl", std::process::id()));
        let opened = std::fs::create_dir_all(&dir).and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        });
        match opened {
            Ok(file) => {
                tracing::info!(
                    target: "luna_harness::opencode",
                    path = %path.display(),
                    "OpenCode wire log enabled"
                );
                Some(WireLog {
                    file: Mutex::new(file),
                    secrets: Mutex::new(Vec::new()),
                })
            }
            Err(error) => {
                tracing::warn!(
                    target: "luna_harness::opencode",
                    path = %path.display(),
                    "OpenCode wire log disabled: {error}"
                );
                None
            }
        }
    })
    .as_ref()
}

/// Never let `secret` (the per-run server password) into the log.
pub(super) fn register_secret(secret: &str) {
    if let Some(log) = wire_log()
        && !secret.is_empty()
        && let Ok(mut secrets) = log.secrets.lock()
    {
        secrets.push(secret.to_owned());
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

impl WireLog {
    fn write(&self, mut record: Value) {
        record["t_ms"] = json!(now_ms() as u64);
        let Ok(mut line) = serde_json::to_string(&record) else {
            return;
        };
        line.push('\n');
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn scrub_text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        if let Ok(secrets) = self.secrets.lock() {
            for secret in secrets.iter() {
                if text.contains(secret.as_str()) {
                    text = text.replace(secret.as_str(), "[redacted]");
                }
            }
        }
        text
    }

    /// A redacted copy: secret-looking keys blanked, registered secrets
    /// replaced inside strings.
    fn redact(&self, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let lowered = key.to_ascii_lowercase();
                        if SECRET_KEYS.contains(&lowered.as_str()) {
                            (key.clone(), json!("[redacted]"))
                        } else {
                            (key.clone(), self.redact(value))
                        }
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.redact(v)).collect()),
            Value::String(text) => Value::String(self.scrub_text(text)),
            other => other.clone(),
        }
    }

    /// `{sha256, bytes, body | text, truncated}` for one body.
    fn body(&self, raw: &[u8]) -> Value {
        if raw.is_empty() {
            return json!({ "bytes": 0 });
        }
        let mut out = json!({ "sha256": sha256_hex(raw), "bytes": raw.len() });
        match serde_json::from_slice::<Value>(raw) {
            Ok(value) if raw.len() <= BODY_LIMIT => out["body"] = self.redact(&value),
            _ => {
                let mut end = raw.len().min(BODY_LIMIT);
                let text = String::from_utf8_lossy(&raw[..end]).into_owned();
                // from_utf8_lossy may lengthen; re-clip on a char boundary.
                end = text.len().min(BODY_LIMIT);
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                out["text"] = json!(self.scrub_text(&text[..end]));
                out["truncated"] = json!(raw.len() > BODY_LIMIT);
            }
        }
        out
    }
}

/// One HTTP exchange. `response` is `Err` for a transport failure.
pub(super) struct HttpRecord<'a> {
    pub(super) server: &'a str,
    pub(super) method: &'a str,
    pub(super) path: &'a str,
    pub(super) request: Option<&'a Value>,
    pub(super) response: Result<(u16, Option<&'a str>), String>,
    pub(super) started: Instant,
}

pub(super) fn log_http(record: HttpRecord<'_>) {
    let Some(log) = wire_log() else {
        return;
    };
    let request = match record.request {
        Some(body) => log.body(serde_json::to_vec(body).unwrap_or_default().as_slice()),
        None => json!({ "bytes": 0 }),
    };
    let mut entry = json!({
        "kind": "http",
        "server": record.server,
        "method": record.method,
        "path": log.scrub_text(record.path),
        "elapsed_ms": record.started.elapsed().as_millis() as u64,
        "request": request,
    });
    match record.response {
        Ok((status, body)) => {
            entry["status"] = json!(status);
            entry["response"] = match body {
                Some(body) => log.body(body.as_bytes()),
                None => json!({ "streamed": true }),
            };
        }
        Err(error) => entry["error"] = json!(log.scrub_text(&error)),
    }
    log.write(entry);
}

/// A bus connection opened (`status`) or failed to open (`error`).
pub(super) fn log_sse_connect(server: &str, generation: u64, outcome: Result<u16, String>) {
    let Some(log) = wire_log() else {
        return;
    };
    let mut entry = json!({ "kind": "sse.connect", "server": server, "generation": generation });
    match outcome {
        Ok(status) => entry["status"] = json!(status),
        Err(error) => entry["error"] = json!(log.scrub_text(&error)),
    }
    log.write(entry);
}

/// A bus stream ended (server closed it, or a transport error).
pub(super) fn log_sse_end(server: &str, generation: u64, reason: &str) {
    if let Some(log) = wire_log() {
        log.write(json!({
            "kind": "sse.end",
            "server": server,
            "generation": generation,
            "reason": reason,
        }));
    }
}

/// One received SSE frame (before normalization).
pub(super) fn log_sse_frame(server: &str, generation: u64, raw: &str, event: &Value) {
    let Some(log) = wire_log() else {
        return;
    };
    log.write(json!({
        "kind": "sse",
        "server": server,
        "generation": generation,
        "id": event.get("id"),
        "type": event.get("type"),
        "session": event.pointer("/data/sessionID"),
        "seq": event.pointer("/durable/seq"),
        "frame": log.body(raw.as_bytes()),
    }));
}
