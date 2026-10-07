//! The SSE event bus: subscription, reconnect and frame decoding.

use super::*;

/// Bus reconnect: the server is our own child on loopback, so a dropped
/// stream with a live process is transient — retry briefly, then treat the
/// run as dead (transcript integrity is gone once frames are missed).
pub(super) const BUS_RECONNECT_DELAY: Duration = Duration::from_millis(250);
pub(super) const BUS_RECONNECT_ATTEMPTS: u32 = 40;

/// What the bus reader hands the session loop.
pub(super) enum BusMsg {
    /// A stream (re)connected and delivered its first frame (2.x always
    /// opens with `server.connected`). The FIRST one gates the initial
    /// prompt: `/api/event` has NO replay, and a fast-failing turn (bad
    /// model id) can run its whole lifecycle before a late subscription
    /// exists. Later ones mean a reconnect gap that may have swallowed our
    /// terminal: the loop re-syncs from `GET /api/session/active`.
    Connected,
    Event(Value),
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

/// Tail the event bus (`GET /api/event`, which serves nothing unless
/// `Accept: text/event-stream` is sent — observed live, 2.0.3) into the
/// session loop. Reconnects on
/// transient drops (the server is our own child on loopback); past the
/// budget the loop learns via [`BusMsg::Disconnected`] and errors the run —
/// missed frames mean the transcript can no longer be trusted.
pub(super) async fn bus_task(base: String, auth: Option<String>, tx: mpsc::Sender<BusMsg>) {
    let client = http_client();
    let url = format!("{base}/api/event");
    let mut failures: u32 = 0;
    // Connection generation: one per accepted subscription (wire log).
    let mut generation: u64 = 0;
    loop {
        if tx.is_closed() {
            return;
        }
        let mut req = client
            .get(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(auth) = &auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                failures = 0;
                generation += 1;
                log_sse_connect(&base, generation, Ok(resp.status().as_u16()));
                stream_bus(&tx, resp, &base, generation).await;
                if tx.is_closed() {
                    return;
                }
            }
            Ok(resp) => log_sse_connect(&base, generation + 1, Ok(resp.status().as_u16())),
            Err(error) => log_sse_connect(&base, generation + 1, Err(error.to_string())),
        }
        // (stream_bus sends Connected itself once the first frame lands —
        // an accepted-but-parked boot-window connection must not count.)
        failures += 1;
        if failures > BUS_RECONNECT_ATTEMPTS {
            let _ = tx.send(BusMsg::Disconnected).await;
            return;
        }
        tokio::time::sleep(BUS_RECONNECT_DELAY).await;
    }
}

pub(super) async fn stream_bus(
    tx: &mpsc::Sender<BusMsg>,
    resp: reqwest::Response,
    base: &str,
    generation: u64,
) {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut announced = false;
    // 2.x names a tool only when its input starts streaming; the later
    // called/success frames carry the call id alone.
    let mut v2_tool_names: HashMap<V2ToolKey, String> = HashMap::new();
    while let Some(chunk) = stream.next().await {
        let Ok(bytes) = chunk else {
            log_sse_end(base, generation, "transport error");
            return;
        };
        if !announced {
            announced = true;
            if tx.send(BusMsg::Connected).await.is_err() {
                return;
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
                note_event_type(event.get("type").and_then(Value::as_str).unwrap_or(""));
                if let Some(signal) = wire_signal(&event)
                    && tx.send(BusMsg::Signal(signal)).await.is_err()
                {
                    return;
                }
                let payloads = normalize_v2_frame(event, &mut v2_tool_names);
                if v2_tool_names.len() > MAX_PENDING_V2_TOOLS {
                    let _ = tx.send(BusMsg::Disconnected).await;
                    return;
                }
                for payload in payloads {
                    if tx.send(BusMsg::Event(payload)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
    log_sse_end(base, generation, "closed");
}
