//! CC-06 delivery receipts and result origin for the Claude stream-json driver.
//!
//! Evidence levels for one stdin user line, weakest first:
//! 1. written — the stdin write succeeded (`SteerAcceptance::Accepted`);
//! 2. recorded — the CLI re-emitted the line with the SAME `uuid` because the
//!    run passes `--replay-user-messages` (2.1.286 `SDKUserMessageReplay`,
//!    `isReplay: true`); this is native incorporation into history, not proof
//!    that a model request sampled it;
//! 3. answered — a `result` echoes the uuid in `user_message_uuid(s)`.
//!
//! Correlation is by native uuid only, never by text. Older CLIs that neither
//! replay nor echo keep the previous behaviour: written stays unconfirmed and
//! a result without echo/origin completes the active turn (T3 `9da066dbe`).
//!
//! A result is FOREIGN when it echoes only other uuids, or echoes none and
//! carries a non-human `origin` (`task-notification`, `peer`, `channel`). A
//! foreign result must not complete a human turn that is still outstanding.

use std::collections::{HashSet, VecDeque};

use serde::Deserialize;
use serde_json::Value;

/// Bound on remembered recorded uuids (duplicate acknowledgments only).
const MAX_REMEMBERED: usize = 256;

/// Add the native `uuid` to a stdin user line built by `wire`.
pub(crate) fn with_uuid(line: String, uuid: &str) -> String {
    match serde_json::from_str::<Value>(&line) {
        Ok(Value::Object(mut map)) => {
            map.insert("uuid".into(), Value::from(uuid));
            Value::Object(map).to_string()
        }
        _ => line,
    }
}

/// A native uuid for an engine message id: a valid UUID is reused so the
/// provider transcript and LUNA share one identity; anything else is mapped to
/// a fresh UUID (the CLI stores these as transcript message ids).
pub(crate) fn native_uuid_for(engine_id: Option<&str>) -> String {
    engine_id
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .map(|id| id.hyphenated().to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// The run's opening prompt.
    Initial,
    /// Ordinary warm routing (follow-up turn fallback retained).
    Routed,
    /// Explicit in-turn steering that needs native incorporation evidence.
    Explicit,
}

#[derive(Debug)]
struct Outbound {
    uuid: String,
    engine_id: Option<String>,
    kind: LineKind,
    recorded: bool,
}

/// What a replay echo proved.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub engine_id: Option<String>,
    pub kind: LineKind,
}

/// Who a `result` frame answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultOwner {
    /// Echoes one of our outstanding uuids.
    Own,
    /// Neither echo nor origin (older runtime): completes the active turn.
    Legacy,
    /// Another native turn (autonomous, peer, channel, or other uuids).
    Foreign,
}

#[derive(Debug, Default)]
pub(crate) struct DeliveryLedger {
    outstanding: VecDeque<Outbound>,
    recorded: VecDeque<String>,
    recorded_set: HashSet<String>,
}

impl DeliveryLedger {
    /// Track a line that was handed to the stdin writer.
    pub fn sent(&mut self, uuid: String, engine_id: Option<String>, kind: LineKind) {
        self.outstanding.push_back(Outbound {
            uuid,
            engine_id,
            kind,
            recorded: false,
        });
    }

    /// Forget a line whose stdin write failed (it never reached the CLI).
    pub fn unsent(&mut self, uuid: &str) {
        self.outstanding.retain(|line| line.uuid != uuid);
    }

    pub fn has_outstanding(&self) -> bool {
        !self.outstanding.is_empty()
    }

    /// A replay echo for `uuid`. `Some` exactly once per outstanding uuid; a
    /// duplicate acknowledgment or an unknown uuid yields `None`.
    pub fn on_replay(&mut self, uuid: &str) -> Option<Recorded> {
        if self.recorded_set.contains(uuid) {
            return None;
        }
        let line = self
            .outstanding
            .iter_mut()
            .find(|line| line.uuid == uuid && !line.recorded)?;
        line.recorded = true;
        let recorded = Recorded {
            engine_id: line.engine_id.clone(),
            kind: line.kind,
        };
        self.remember(uuid.to_owned());
        Some(recorded)
    }

    fn remember(&mut self, uuid: String) {
        if self.recorded_set.insert(uuid.clone()) {
            self.recorded.push_back(uuid);
        }
        while self.recorded.len() > MAX_REMEMBERED {
            if let Some(old) = self.recorded.pop_front() {
                self.recorded_set.remove(&old);
            }
        }
    }

    /// Classify a result and retire the outstanding lines it answers.
    pub fn classify(&mut self, echo: &ResultEcho) -> ResultOwner {
        let echoed = echo.echoed();
        if !echoed.is_empty() {
            // stdin is consumed in order: an answer to a later line also
            // retires every earlier line, even if the CLI merged it silently.
            let last = self
                .outstanding
                .iter()
                .rposition(|line| echoed.contains(&line.uuid.as_str()));
            return match last {
                Some(index) => {
                    self.outstanding.drain(..=index);
                    ResultOwner::Own
                }
                None => ResultOwner::Foreign,
            };
        }
        if echo.origin_kind().is_some_and(|kind| kind != "human") {
            return ResultOwner::Foreign;
        }
        // Older runtime: the oldest outstanding line owns this result.
        self.outstanding.pop_front();
        ResultOwner::Legacy
    }

    /// An interrupt ends every outstanding claim; the interrupted Done is
    /// authoritative for the run.
    pub fn clear(&mut self) {
        self.outstanding.clear();
    }
}

/// The correlation fields of a `result` frame (2.1.286 `SDKResultMessage`).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ResultEcho {
    #[serde(default)]
    pub user_message_uuid: Option<String>,
    #[serde(default)]
    pub user_message_uuids: Vec<String>,
    #[serde(default)]
    pub origin: Option<Value>,
}

impl ResultEcho {
    pub fn echoed(&self) -> Vec<&str> {
        self.user_message_uuids
            .iter()
            .map(String::as_str)
            .chain(self.user_message_uuid.as_deref())
            .filter(|id| !id.is_empty())
            .collect()
    }

    pub fn origin_kind(&self) -> Option<&str> {
        self.origin.as_ref()?.get("kind")?.as_str()
    }
}

/// A stdout line the delivery/accounting/control side channel consumes before
/// ordinary normalization.
#[derive(Debug)]
pub(crate) enum SideFrame {
    /// `{"type":"user","isReplay":true,"uuid":…}` — never transcript content.
    Replay { uuid: String },
    /// `{"type":"control_response","response":{"request_id":…}}`.
    ControlResponse(Value),
}

/// Cheap pre-filter, then a typed peek. Lines that are not side frames go on
/// to `wire::parse_frame` unchanged.
pub(crate) fn side_frame(line: &str) -> Option<SideFrame> {
    let replay = line.contains("\"isReplay\"");
    let control = line.contains("\"control_response\"");
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
        "control_response" => Some(SideFrame::ControlResponse(value)),
        _ => None,
    }
}
