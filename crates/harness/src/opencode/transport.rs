//! The SSE event bus: subscription, reconnect, durable-seq tracking and
//! gap detection, read-idle detection and frame decoding (OC-08).
//!
//! `/api/event` has no replay. Each durable frame carries `durable:
//! {aggregateID, seq}` (aggregate = session); the bus remembers the last
//! seq per session, drops frames at or below it (duplicates), and after a
//! reconnect asks the session's durable log (`…/log?after=<seq>`) for what
//! was missed before any live frame is forwarded. The live bus itself
//! skips some durable seqs (observed live on 2.0.22), so only a reconnect,
//! not a mid-stream seq jump, counts as a gap.

use super::*;

/// Bus reconnect: the server is our own child on loopback, so a dropped
/// stream with a live process is transient — retry briefly, then treat the
/// run as dead.
pub(super) const BUS_RECONNECT_DELAY: Duration = Duration::from_millis(250);
pub(super) const BUS_RECONNECT_ATTEMPTS: u32 = 40;
/// v2 writes `: heartbeat` every 15 s: three missed beats mean the stream
/// is dead even though the socket is open.
pub(super) const BUS_READ_IDLE: Duration = Duration::from_secs(45);
/// Sessions whose last seq is remembered (the root and its children).
const MAX_TRACKED_SESSIONS: usize = 256;
/// Bound on one durable-log read after a reconnect.
const LOG_READ_BUDGET: Duration = Duration::from_secs(10);
const MAX_LOG_FRAMES: usize = 4096;

/// What the bus reader hands the session loop.
pub(super) enum BusMsg {
    /// The first subscription delivered its first frame (2.x always opens
    /// with `server.connected`). It gates the initial prompt: `/api/event`
    /// has NO replay, and a fast-failing turn (bad model id) can run its
    /// whole lifecycle before a late subscription exists.
    Connected,
    /// A later subscription is live. Whatever the durable log replayed was
    /// forwarded before this message; `gap` says whether frames are still
    /// missing (the loop reads them back from the messages).
    Reconnected {
        generation: u64,
        gap: ReplayGap,
    },
    Event(Value),
    /// A permission or form request opened (`pending`) or settled, by id
    /// (raw `permission.asked/.replied`, `form.created/.replied/.cancelled`).
    Request {
        id: String,
        session: String,
        pending: bool,
    },
    /// The stream is gone past the reconnect budget (or the reader saw the
    /// consumer close).
    Disconnected,
    /// A slash command's synchronous `POST …/command` failed (a tagged
    /// 4xx/5xx or a transport error). Not a bus frame: the detached command
    /// task reports it here so the turn settles instead of stalling.
    CommandFailed(String),
    /// A raw frame the loop needs besides the normalized payloads: LUNA tool
    /// evidence or a catalog change ([`wire_signal`]). Sent before that
    /// frame's normalized payloads.
    Signal(WireSignal),
}

/// What a reconnect's durable-log read found for the root session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReplayGap {
    /// The log watermark equals the last seq seen: nothing was missed.
    None,
    /// The log carried every missed durable frame (forwarded already).
    Replayed(usize),
    /// Frames were committed after `after` and the log did not carry them
    /// (2.0.22 keeps only the watermark).
    Missed {
        after: Option<u64>,
        watermark: Option<u64>,
    },
    /// The log could not be read.
    Unknown(String),
}

impl ReplayGap {
    pub(super) fn needs_readback(&self) -> bool {
        matches!(self, Self::Missed { .. } | Self::Unknown(_))
    }

    pub(super) fn describe(&self) -> Value {
        match self {
            Self::None => json!({ "gap": "none" }),
            Self::Replayed(count) => json!({ "gap": "replayed", "frames": count }),
            Self::Missed { after, watermark } => {
                json!({ "gap": "missed", "after": after, "watermark": watermark })
            }
            Self::Unknown(error) => json!({ "gap": "unknown", "error": error }),
        }
    }
}

/// Where the bus connects and which session its replay covers.
#[derive(Clone)]
pub(super) struct BusConfig {
    pub(super) base: String,
    pub(super) auth: Option<String>,
    pub(super) session_id: String,
    pub(super) directory: Option<String>,
}

/// The last durable seq seen per session.
#[derive(Default)]
pub(super) struct SeqTracker {
    last: HashMap<String, u64>,
    order: VecDeque<String>,
}

impl SeqTracker {
    pub(super) fn last(&self, aggregate: &str) -> Option<u64> {
        self.last.get(aggregate).copied()
    }

    /// `false` for a duplicate (at or below the last seq seen).
    pub(super) fn observe(&mut self, aggregate: &str, seq: u64) -> bool {
        match self.last.get_mut(aggregate) {
            Some(last) if seq <= *last => false,
            Some(last) => {
                *last = seq;
                true
            }
            None => {
                if self.order.len() >= MAX_TRACKED_SESSIONS
                    && let Some(oldest) = self.order.pop_front()
                {
                    self.last.remove(&oldest);
                }
                self.order.push_back(aggregate.to_owned());
                self.last.insert(aggregate.to_owned(), seq);
                true
            }
        }
    }
}

/// `(aggregateID, seq)` of a durable frame.
pub(super) fn durable_position(event: &Value) -> Option<(&str, u64)> {
    let durable = event.get("durable")?;
    Some((
        durable.get("aggregateID").and_then(Value::as_str)?,
        durable.get("seq").and_then(Value::as_u64)?,
    ))
}

/// Raw request lifecycle frames: `(id, session, pending)`.
fn request_lifecycle(event: &Value) -> Option<(String, String, bool)> {
    let kind = event.get("type").and_then(Value::as_str)?;
    let data = event.get("data")?;
    let (id, session, pending) = match kind {
        "permission.asked" => (data.get("id")?, data.get("sessionID")?, true),
        "permission.replied" => (data.get("requestID")?, data.get("sessionID")?, false),
        "form.created" => (
            data.pointer("/form/id")?,
            data.pointer("/form/sessionID")?,
            true,
        ),
        "form.replied" | "form.cancelled" => (data.get("id")?, data.get("sessionID")?, false),
        _ => return None,
    };
    Some((
        id.as_str()?.to_owned(),
        session.as_str()?.to_owned(),
        pending,
    ))
}

/// Per-bus state that outlives one connection.
struct BusState {
    generation: u64,
    seqs: SeqTracker,
    /// 2.x names a tool only when its input starts streaming; the later
    /// called/success frames carry the call id alone.
    tool_names: HashMap<V2ToolKey, String>,
}

/// Tail the event bus (`GET /api/event`, which serves nothing unless
/// `Accept: text/event-stream` is sent — observed live, 2.0.3) into the
/// session loop. Reconnects on transient drops (the server is our own child
/// on loopback); past the budget the loop learns via
/// [`BusMsg::Disconnected`].
pub(super) async fn bus_task(config: BusConfig, tx: mpsc::Sender<BusMsg>) {
    let client = http_client();
    let url = format!("{}/api/event", config.base);
    let mut failures: u32 = 0;
    let mut state = BusState {
        generation: 0,
        seqs: SeqTracker::default(),
        tool_names: HashMap::new(),
    };
    loop {
        if tx.is_closed() {
            return;
        }
        let mut req = client
            .get(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(auth) = &config.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let previous_generation = state.generation;
                log_sse_connect(
                    &config.base,
                    state.generation + 1,
                    Ok(resp.status().as_u16()),
                );
                if !stream_bus(&tx, resp, &config, &client, &mut state).await {
                    return;
                }
                // A 200 whose body never arrived was not a live subscription.
                // Keep its failure in the retry budget and preserve Connected
                // for the first attempt that actually receives SSE bytes.
                if state.generation != previous_generation {
                    failures = 0;
                }
                if tx.is_closed() {
                    return;
                }
            }
            Ok(resp) => log_sse_connect(
                &config.base,
                state.generation + 1,
                Ok(resp.status().as_u16()),
            ),
            Err(error) => {
                log_sse_connect(&config.base, state.generation + 1, Err(error.to_string()))
            }
        }
        // (stream_bus announces the connection itself once the first frame
        // lands — an accepted-but-parked boot-window connection must not
        // count.)
        failures += 1;
        if failures > BUS_RECONNECT_ATTEMPTS {
            let _ = tx.send(BusMsg::Disconnected).await;
            return;
        }
        tokio::time::sleep(BUS_RECONNECT_DELAY).await;
    }
}

/// Forward one decoded frame: duplicate check, request lifecycle, then the
/// normalized payloads. `false` when the loop is gone (or the pending-tool
/// bound broke).
async fn forward_frame(tx: &mpsc::Sender<BusMsg>, state: &mut BusState, event: Value) -> bool {
    if let Some((aggregate, seq)) = durable_position(&event)
        && !state.seqs.observe(aggregate, seq)
    {
        return true;
    }
    note_event_type(event.get("type").and_then(Value::as_str).unwrap_or(""));
    // Raw LUNA-tool evidence and catalog changes (OC-13/OC-14) reach the
    // loop BEFORE the frame's normalized payloads.
    if let Some(signal) = wire_signal(&event)
        && tx.send(BusMsg::Signal(signal)).await.is_err()
    {
        return false;
    }
    if let Some((id, session, pending)) = request_lifecycle(&event)
        && tx
            .send(BusMsg::Request {
                id,
                session,
                pending,
            })
            .await
            .is_err()
    {
        return false;
    }
    let payloads = normalize_v2_frame(event, &mut state.tool_names);
    if state.tool_names.len() > MAX_PENDING_V2_TOOLS {
        let _ = tx.send(BusMsg::Disconnected).await;
        return false;
    }
    for payload in payloads {
        if tx.send(BusMsg::Event(payload)).await.is_err() {
            return false;
        }
    }
    true
}

/// Read one subscription until it ends. `false` when the bus should stop
/// (the loop is gone).
async fn stream_bus(
    tx: &mpsc::Sender<BusMsg>,
    resp: reqwest::Response,
    config: &BusConfig,
    client: &reqwest::Client,
    state: &mut BusState,
) -> bool {
    let base = config.base.as_str();
    let generation = state.generation + 1;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut announced = false;
    loop {
        let chunk = match tokio::time::timeout(BUS_READ_IDLE, stream.next()).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => {
                log_sse_end(base, generation, "closed");
                return true;
            }
            Err(_) => {
                log_sse_end(base, generation, "read idle (no heartbeat)");
                return true;
            }
        };
        let Ok(bytes) = chunk else {
            log_sse_end(base, generation, "transport error");
            return true;
        };
        if bytes.is_empty() {
            continue;
        }
        if !announced {
            announced = true;
            state.generation = generation;
            let msg = if generation == 1 {
                BusMsg::Connected
            } else {
                // Before any live frame of this connection: forward what
                // the durable log still has, then report the gap.
                let gap = replay_log(tx, config, client, state).await;
                BusMsg::Reconnected { generation, gap }
            };
            if tx.send(msg).await.is_err() {
                return false;
            }
        }
        buf.extend_from_slice(&bytes);
        // SSE frames are blank-line separated; each data line is one event.
        while let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
            let frame: Vec<u8> = buf.drain(..pos + 2).collect();
            let Ok(frame) = std::str::from_utf8(&frame) else {
                continue;
            };
            for line in frame.lines() {
                let Some(data) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
                else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                log_sse_frame(base, generation, data, &event);
                if !forward_frame(tx, state, event).await {
                    return false;
                }
            }
        }
    }
}

/// After a reconnect: read the root session's durable log after the last
/// seq seen, forward any frame it still carries, and compare its watermark
/// (`log.synced{seq}`) with what the bus has now seen.
async fn replay_log(
    tx: &mpsc::Sender<BusMsg>,
    config: &BusConfig,
    client: &reqwest::Client,
    state: &mut BusState,
) -> ReplayGap {
    let session = config.session_id.as_str();
    let after = state.seqs.last(session);
    let mut url = format!("{}/api/experimental/session/{session}/log", config.base);
    if let Some(after) = after {
        url.push_str(&format!("?after={after}"));
    }
    let mut req = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "text/event-stream");
    if let Some(auth) = &config.auth {
        req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
    }
    if let Some(dir) = &config.directory {
        req = req.header("x-opencode-directory", encode_directory(dir));
    }
    let log_path = url
        .strip_prefix(config.base.as_str())
        .unwrap_or(url.as_str())
        .to_owned();
    let started = Instant::now();
    let read = async {
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(error) => {
                log_http(HttpRecord {
                    server: &config.base,
                    method: "GET",
                    path: &log_path,
                    request: None,
                    response: Err(error.to_string()),
                    started,
                });
                return Err(error.to_string());
            }
        };
        log_http(HttpRecord {
            server: &config.base,
            method: "GET",
            path: &log_path,
            request: None,
            response: Ok((resp.status().as_u16(), None)),
            started,
        });
        if !resp.status().is_success() {
            return Err(format!("session log answered {}", resp.status()));
        }
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut forwarded = 0usize;
        let mut seen = 0usize;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|error| error.to_string())?;
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
                let frame: Vec<u8> = buf.drain(..pos + 2).collect();
                let Ok(frame) = std::str::from_utf8(&frame) else {
                    continue;
                };
                for line in frame.lines() {
                    let Some(data) = line
                        .strip_prefix("data: ")
                        .or_else(|| line.strip_prefix("data:"))
                    else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    if event.get("type").and_then(Value::as_str) == Some("log.synced") {
                        return Ok((forwarded, event.get("seq").and_then(Value::as_u64)));
                    }
                    seen += 1;
                    if seen > MAX_LOG_FRAMES {
                        return Err("session log replay exceeded its frame bound".into());
                    }
                    let fresh = durable_position(&event)
                        .is_some_and(|(aggregate, seq)| state.seqs.last(aggregate) < Some(seq));
                    if fresh {
                        log_sse_frame(&config.base, state.generation, data, &event);
                        if !forward_frame(tx, state, event).await {
                            return Err("the run loop is gone".into());
                        }
                        forwarded += 1;
                    }
                }
            }
        }
        Ok((forwarded, None))
    };
    match tokio::time::timeout(LOG_READ_BUDGET, read).await {
        Err(_) => ReplayGap::Unknown("session log read timed out".into()),
        Ok(Err(error)) => ReplayGap::Unknown(error),
        Ok(Ok((forwarded, watermark))) => {
            let now = state.seqs.last(session);
            match watermark {
                Some(watermark) if now.is_none_or(|now| now < watermark) => ReplayGap::Missed {
                    after: now,
                    watermark: Some(watermark),
                },
                _ if forwarded > 0 => ReplayGap::Replayed(forwarded),
                _ => ReplayGap::None,
            }
        }
    }
}
