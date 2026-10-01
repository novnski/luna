//! CC-06 side frames for the Claude stream-json driver.
//!
//! The run passes `--replay-user-messages`, so the CLI re-emits each stdin
//! user line with the SAME `uuid` once it records it (2.1.286
//! `SDKUserMessageReplay`, `isReplay: true`). That replay is a native
//! delivery receipt, folded into the single uuid/receipt mechanism of
//! `transport::ControlChannel` (one native uuid per send, minted there;
//! explicit steers settle once, on replay or lifecycle `started`). Result
//! ownership (foreign results never completing a human turn) is the CC-02
//! turn ledger's (`turns.rs`).
//!
//! Correlation is by native uuid only, never by text.

use serde_json::Value;

use super::runtime_controls::ID_PREFIX;

/// A stdout line the CC-06 side channel consumes before ordinary
/// normalization.
#[derive(Debug)]
pub(crate) enum SideFrame {
    /// `{"type":"user","isReplay":true,"uuid":…}` — never transcript content.
    Replay { uuid: String },
    /// A `control_response` to one of CC-06's own `luna-cc06-` requests.
    /// Every other reply belongs to the transport.
    ControlResponse(Value),
}

/// Cheap pre-filter, then a typed peek. Lines that are not side frames go on
/// to the transport and `wire::parse_frame` unchanged.
pub(crate) fn side_frame(line: &str) -> Option<SideFrame> {
    let replay = line.contains("\"isReplay\"");
    let control = line.contains("\"control_response\"") && line.contains(ID_PREFIX);
    if !replay && !control {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "user" if value.get("isReplay").and_then(Value::as_bool) == Some(true) => {
            // Replays of CLI-originated content (local command output, merged
            // synthetic input) carry uuids we never sent; they are still not
            // conversation content for LUNA, so consume them either way.
            let uuid = value
                .get("uuid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Some(SideFrame::Replay { uuid })
        }
        "control_response"
            if value
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with(ID_PREFIX)) =>
        {
            Some(SideFrame::ControlResponse(value))
        }
        _ => None,
    }
}
