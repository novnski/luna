//! RegistryClient — WebSocket transport for the workspace registry
//! (docs/registry-sync.md): hello/cursor handshake, push/ack for pending op
//! batches, merged-row broadcasts, presence beats, probe/redial liveness, and
//! reconnect with exponential backoff.
//!
//! The client owns no row semantics: everything applies through the shared
//! [`luna_doc::RegistryDoc`] under a lock. Wire frames are JSON text —
//! byte-compatible with `edge/src/registry-room.ts`.
//!
//! Liveness discipline is inherited from `room.rs` and its incidents: the
//! transport-level text ping elicits a runtime auto-pong that proves NOTHING
//! about the DO (2026-07-30), so room-level health is judged only by protocol
//! frames — a probe that goes unanswered past its deadline tears the session
//! down for a fresh dial.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use luna_doc::{PendingBatch, RegistryDoc, RegistryRow, StateOutcome};

use crate::types::{RoomStatsSnapshot, StaticUrl, SyncError, UrlProvider};
use crate::write_epoch::EpochError;

/// Text `"ping"` keepalive interval (answered by the DO auto-response pair
/// without waking it — transport liveness only).
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// Transport silence lease: pongs count, so a healthy socket never trips this.
const SILENCE_LEASE: Duration = Duration::from_secs(45);
/// Bound on one dial attempt (URL fetch + WS handshake).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// The server must answer a hello with `state` within this deadline.
const HELLO_DEADLINE: Duration = Duration::from_secs(15);
/// A probe's `probe-ok` (or any other protocol frame) must arrive within this
/// deadline, or the session is torn down for a fresh socket.
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
/// Presence entries older than this are treated as expired (mirrors the
/// EphemeralStore's 30s TTL the old workspace room used).
const PRESENCE_TTL: Duration = Duration::from_secs(30);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Worst-case dark window after the network returns (event wakes usually
/// beat this; the cap only matters when every event path missed).
const BACKOFF_CAP: Duration = Duration::from_secs(16);
/// A joined session must survive this long before a disconnect resets the
/// backoff to base. Resetting on join alone lets a connect-and-die socket
/// (captive portal, half-broken NAT) hot-loop at 250ms forever.
const STABLE_RESET: Duration = Duration::from_secs(30);
/// Safety re-check cadence while parked on "OS says offline" — a stuck or
/// wrong path monitor degrades to slow polling, never to silence.
const OFFLINE_PARK_RECHECK: Duration = Duration::from_secs(30);

/// Quiet-room probe cadence default. The workspace registry is one room per
/// engine, so a fixed 15min cadence costs ~100 DO wakes/day total.
const PROBE_QUIET_DEFAULT: Duration = Duration::from_secs(900);

/// Per-client tuning.
#[derive(Clone, Copy, Debug)]
pub struct RegistryTuning {
    /// Send a liveness probe after this much protocol-frame silence.
    pub probe_quiet: Duration,
}

impl Default for RegistryTuning {
    fn default() -> Self {
        Self {
            probe_quiet: PROBE_QUIET_DEFAULT,
        }
    }
}

/// Connection/sync lifecycle notifications (best-effort broadcast).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryEvent {
    /// Joined (or re-joined); the hello state has been applied.
    Connected,
    /// The connection dropped; the client is backing off before redialing.
    Disconnected,
    /// Rows/acks were applied to the doc — republish and persist.
    Applied,
    /// A remote device's presence beat arrived.
    Presence,
}

// ── wire frames (JSON text; mirror edge/src/registry-room.ts) ───────────────

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum ClientFrame<'a> {
    Hello {
        cursor: Option<u64>,
        device: &'a str,
    },
    Push {
        batch: &'a str,
        ops: &'a [luna_doc::RowOp],
    },
    Presence {
        at: i64,
    },
    Probe,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum ServerFrame {
    State {
        seq: u64,
        full: bool,
        #[serde(rename = "gcFloor", default)]
        gc_floor: u64,
        rows: Vec<RegistryRow>,
        #[serde(default)]
        presence: HashMap<String, i64>,
    },
    Rows {
        seq: u64,
        rows: Vec<RegistryRow>,
    },
    Ack {
        batch: String,
        seq: u64,
        #[allow(dead_code)]
        applied: u64,
    },
    Presence {
        device: String,
        at: i64,
    },
    #[serde(rename = "probe-ok")]
    ProbeOk {
        #[allow(dead_code)]
        seq: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

// ── transport plumbing (text-frame sibling of room.rs's Pipe) ───────────────

pub(crate) struct TextPipe {
    pub(crate) tx: mpsc::Sender<String>,
    pub(crate) rx: mpsc::Receiver<String>,
}

pub(crate) trait TextConnector: Send + Sync + 'static {
    fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>>;
}

/// Plain-HTTPS pull/push seam — the airplane-wifi transport. `fetch` GETs
/// `/registry/{org}/rows?since=` (the WS hello's exact delta answer as JSON);
/// `push` POSTs one op batch to `/registry/{org}/push` and returns the ack
/// body. Both are safe at-least-once: LWW clocks make replays apply zero ops.
pub trait RegistryTransport: Send + Sync + 'static {
    fn fetch(&self, since: u64) -> BoxFuture<'static, Result<String, SyncError>>;
    fn push(&self, body: String) -> BoxFuture<'static, Result<String, SyncError>>;
}

struct WsTextConnector {
    url: Arc<dyn UrlProvider>,
}

impl TextConnector for WsTextConnector {
    fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>> {
        let provider = self.url.clone();
        Box::pin(async move {
            let url = provider.url().await?;
            let ws = crate::dial::connect_ws(&url)
                .await
                .map_err(|e| SyncError::WebSocket(e.to_string()))?;
            let (out_tx, out_rx) = mpsc::channel(64);
            let (in_tx, in_rx) = mpsc::channel(64);
            tokio::spawn(pump(ws, out_rx, in_tx));
            Ok(TextPipe {
                tx: out_tx,
                rx: in_rx,
            })
        })
    }
}

/// Shuttle text frames between the WebSocket and the actor's channels, plus
/// the ping keepalive and the transport silence lease.
async fn pump(
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    mut out_rx: mpsc::Receiver<String>,
    in_tx: mpsc::Sender<String>,
) {
    let (mut sink, mut stream) = ws.split();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            frame = out_rx.recv() => match frame {
                Some(text) => {
                    if sink.send(WsMessage::Text(text)).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sink.send(WsMessage::Close(None)).await;
                    break;
                }
            },
            frame = stream.next() => match frame {
                Some(Ok(WsMessage::Text(text))) => {
                    last_rx = tokio::time::Instant::now();
                    let text = text.to_string();
                    if text != "pong" && in_tx.send(text).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {
                    last_rx = tokio::time::Instant::now();
                }
                Some(Err(_)) | None => break,
            },
            _ = ping.tick() => {
                if sink.send(WsMessage::Text("ping".into())).await.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(last_rx + SILENCE_LEASE) => {
                tracing::warn!("registry socket silent past lease; treating as dead");
                break;
            }
        }
    }
}

// ── stats (RoomStatsSnapshot-compatible so SyncStatus/`luna sync` render it) ─

#[derive(Default)]
struct Stats {
    connected: std::sync::atomic::AtomicBool,
    /// Latched once a server state applies this process (hello or HTTPS
    /// pull) — never cleared by disconnects; see `RoomStatsSnapshot::synced`.
    synced: std::sync::atomic::AtomicBool,
    last_pushed_ms: std::sync::atomic::AtomicI64,
    last_ack_ms: std::sync::atomic::AtomicI64,
    rejoins: std::sync::atomic::AtomicU64,
    probes: std::sync::atomic::AtomicU64,
    full_resyncs: std::sync::atomic::AtomicU64,
    disconnects: std::sync::atomic::AtomicU64,
    rejected: std::sync::atomic::AtomicU64,
    /// Epoch ms of the next scheduled dial attempt (0 = connected or dialing
    /// right now). Drives the "Reconnecting — retrying in Ns" countdown.
    retry_at_ms: std::sync::atomic::AtomicI64,
    /// Monotonic dial-attempt counter; each attempt's number is its trace id
    /// in logs, so an incident reads as one numbered sequence.
    dial_seq: std::sync::atomic::AtomicU64,
    /// The failure that started/extended the current outage. Sticky: cleared
    /// only by a successful join, so the UI can keep the last failure visible
    /// through the next attempt instead of flickering to "connecting…".
    last_failure: Mutex<Option<String>>,
}

/// One room's reconnect posture, for connection-status UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconnectState {
    /// Epoch ms of the next scheduled dial (0 = none pending).
    pub retry_at_ms: i64,
    /// Failure that started the current outage; `None` once rejoined.
    pub last_failure: Option<String>,
}

impl Stats {
    fn snapshot(&self) -> RoomStatsSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        RoomStatsSnapshot {
            connected: self.connected.load(Relaxed),
            synced: self.synced.load(Relaxed),
            last_pushed_ms: self.last_pushed_ms.load(Relaxed),
            last_ack_ms: self.last_ack_ms.load(Relaxed),
            rejoins: self.rejoins.load(Relaxed),
            probes: self.probes.load(Relaxed),
            full_resyncs: self.full_resyncs.load(Relaxed),
            disconnects: self.disconnects.load(Relaxed),
            rejected: self.rejected.load(Relaxed),
        }
    }
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A drop-only ticket. Product callbacks must keep both the write epoch and
/// device-root lease alive inside this box; the sync crate cannot inspect it.
pub type RegistryWritePermit = Box<dyn Send>;
/// Called before any mutation of the shared registry document. The engine can
/// supply a weak owner handle without making this crate depend on the engine.
pub type RegistryAdmission = Arc<dyn Fn() -> Result<RegistryWritePermit, EpochError> + Send + Sync>;

#[derive(Clone)]
struct DocWriter {
    doc: Arc<Mutex<RegistryDoc>>,
    admission: Option<RegistryAdmission>,
    retry_flight_reset: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
}

impl DocWriter {
    fn mutate<R>(&self, f: impl FnOnce(&mut RegistryDoc) -> R) -> Result<R, EpochError> {
        let _permit = self
            .admission
            .as_ref()
            .map(|admit| admit())
            .transpose()
            .inspect_err(|error| {
                if *error == EpochError::Retired {
                    self.shutdown.send_replace(true);
                }
            })?;
        Ok(f(&mut lock(&self.doc)))
    }

    fn mark_disconnected(&self) -> Result<(), EpochError> {
        match self.mutate(RegistryDoc::mark_disconnected) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.retry_flight_reset.store(true, Ordering::Release);
                Err(error)
            }
        }
    }

    fn take_pushable(&self) -> Result<Vec<PendingBatch>, EpochError> {
        self.mutate(|doc| {
            if self.retry_flight_reset.swap(false, Ordering::AcqRel) {
                doc.mark_disconnected();
            }
            doc.take_pushable()
        })
    }

    fn defer_flight_reset(&self) {
        self.retry_flight_reset.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct OfflineTaskState {
    stopping: bool,
    handle: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
struct OfflineTasks(Mutex<OfflineTaskState>);

impl OfflineTasks {
    fn stop(&self) -> Option<tokio::task::JoinHandle<()>> {
        let mut state = lock(&self.0);
        state.stopping = true;
        state.handle.take()
    }

    /// Stop an actor-owned child without consuming its handle. The client or
    /// pending connection must still be able to take and await that handle.
    fn abort(&self) {
        let mut state = lock(&self.0);
        state.stopping = true;
        if let Some(child) = &state.handle {
            child.abort();
        }
    }
}

/// Aborts an actor if construction is canceled before it returns a client.
/// Dropping the constructor future, even during `abort_and_join`, only aborts
/// these tasks; only an awaited cancellation proves they have exited.
struct PendingConnection {
    task: Option<tokio::task::JoinHandle<()>>,
    offline: Arc<OfflineTasks>,
    child: Option<tokio::task::JoinHandle<()>>,
}

impl PendingConnection {
    async fn abort_and_join(&mut self) {
        if self.child.is_none() {
            self.child = self.offline.stop();
        }
        if let Some(child) = &self.child {
            child.abort();
        }
        if let Some(task) = self.task.as_mut() {
            task.abort();
            let _ = task.await;
        }
        drop(self.task.take());
        if let Some(child) = self.child.as_mut() {
            let _ = child.await;
        }
        drop(self.child.take());
    }

    async fn ready_or_stop<F>(
        &mut self,
        ready_rx: oneshot::Receiver<Result<(), SyncError>>,
        stop: F,
    ) -> Result<(), SyncError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::pin!(stop);
        let ready = tokio::select! {
            biased;
            _ = &mut stop => {
                self.abort_and_join().await;
                return Err(SyncError::Closed);
            }
            ready = ready_rx => ready,
        };
        match ready {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                self.abort_and_join().await;
                Err(err)
            }
            Err(_) => {
                self.abort_and_join().await;
                Err(SyncError::Closed)
            }
        }
    }
}

impl Drop for PendingConnection {
    fn drop(&mut self) {
        if self.task.is_none() && self.child.is_none() {
            return;
        }
        let child = self.child.take().or_else(|| self.offline.stop());
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(child) = child {
            child.abort();
        }
    }
}

// ── the client ──────────────────────────────────────────────────────────────

/// A live registry-room membership for one [`RegistryDoc`].
///
/// Owns a background actor that keeps the doc converged with the room. The
/// engine host mutates the doc under its own lock and calls [`Self::nudge`]
/// to push; server frames apply through the same lock.
pub struct RegistryClient {
    doc: Arc<Mutex<RegistryDoc>>,
    events: broadcast::Sender<RegistryEvent>,
    shutdown: watch::Sender<bool>,
    nudge: mpsc::Sender<()>,
    probe: mpsc::Sender<()>,
    redial: mpsc::Sender<()>,
    presence_out: mpsc::Sender<i64>,
    presence: Arc<Mutex<HashMap<String, (i64, tokio::time::Instant)>>>,
    stats: Arc<Stats>,
    task: Option<tokio::task::JoinHandle<()>>,
    offline: Arc<OfflineTasks>,
}

/// Handles returned by an immediate registry revocation. Keep this token until
/// [`Self::join`] finishes so the actor and HTTPS child have actually exited.
#[must_use = "await join() to finish draining the registry tasks"]
pub struct RegistryDrain {
    actor: Option<tokio::task::JoinHandle<()>>,
    offline: Option<tokio::task::JoinHandle<()>>,
}

impl RegistryDrain {
    /// Await both aborted tasks without holding the registry document lock.
    /// If this future is canceled, `Drop` aborts whichever handle remains.
    pub async fn join(mut self) {
        if let Some(actor) = self.actor.as_mut() {
            let _ = actor.await;
        }
        drop(self.actor.take());
        if let Some(offline) = self.offline.as_mut() {
            let _ = offline.await;
        }
        drop(self.offline.take());
    }
}

impl Drop for RegistryDrain {
    fn drop(&mut self) {
        if let Some(actor) = self.actor.take() {
            actor.abort();
        }
        if let Some(offline) = self.offline.take() {
            offline.abort();
        }
    }
}

impl RegistryClient {
    /// Connect (fixed URL — dev/tests).
    pub async fn connect(
        url: &str,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
    ) -> Result<Self, SyncError> {
        Self::connect_via(Arc::new(StaticUrl(url.to_string())), doc, device_id).await
    }

    /// Connect with a per-dial URL provider (fresh `?token=` every attempt).
    /// Resolves once the initial hello/state handshake lands; a first-attempt
    /// failure is returned as `Err` (callers own the initial-join retry, same
    /// contract as `ChatClient`). After that the client reconnects itself.
    pub async fn connect_via(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
    ) -> Result<Self, SyncError> {
        Self::connect_via_tuned(provider, doc, device_id, RegistryTuning::default()).await
    }

    pub async fn connect_via_tuned(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport(connector, doc, device_id, tuning, None).await
    }

    /// Guarded WebSocket membership. Every shared-doc mutation must obtain an
    /// owner-held ticket; denied admission never falls back to legacy mode.
    pub async fn connect_via_tuned_guarded(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        admission: RegistryAdmission,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport_mode(connector, doc, device_id, tuning, None, Some(admission))
            .await
    }

    /// Guarded WebSocket membership with an awaited initial-join stop path.
    /// The caller must await this future when signaling `stop`: externally
    /// dropping it only triggers PendingConnection's abort-on-drop fallback.
    pub async fn connect_via_tuned_guarded_until<F>(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        admission: RegistryAdmission,
        stop: F,
    ) -> Result<Self, SyncError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport_mode_until(
            connector,
            doc,
            device_id,
            tuning,
            None,
            Some(admission),
            stop,
        )
        .await
    }

    /// Connect with a plain-HTTPS pull/push seam alongside the socket (the
    /// airplane-wifi transport): an immediate delta GET bootstraps the doc
    /// in ~1 RTT while the WS dials, and every backoff cycle syncs over
    /// HTTPS so a network that never passes the upgrade still converges.
    pub async fn connect_via_transport(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Arc<dyn RegistryTransport>,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport(connector, doc, device_id, tuning, Some(transport)).await
    }

    /// Guarded dual transport; the HTTPS child uses the same admission as the
    /// socket actor and is canceled with this client.
    pub async fn connect_via_transport_guarded(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Arc<dyn RegistryTransport>,
        admission: RegistryAdmission,
    ) -> Result<Self, SyncError> {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport_mode(
            connector,
            doc,
            device_id,
            tuning,
            Some(transport),
            Some(admission),
        )
        .await
    }

    /// Guarded dual transport with an awaited initial-join stop path. The
    /// HTTPS child and socket actor are joined before a stop returns `Closed`.
    /// External future drop remains abort-only.
    pub async fn connect_via_transport_guarded_until<F>(
        provider: Arc<dyn UrlProvider>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Arc<dyn RegistryTransport>,
        admission: RegistryAdmission,
        stop: F,
    ) -> Result<Self, SyncError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let connector = Arc::new(WsTextConnector { url: provider });
        Self::connect_with_transport_mode_until(
            connector,
            doc,
            device_id,
            tuning,
            Some(transport),
            Some(admission),
            stop,
        )
        .await
    }

    pub(crate) async fn connect_with_tuned(
        connector: Arc<dyn TextConnector>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
    ) -> Result<Self, SyncError> {
        Self::connect_with_transport(connector, doc, device_id, tuning, None).await
    }

    pub(crate) async fn connect_with_transport(
        connector: Arc<dyn TextConnector>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Option<Arc<dyn RegistryTransport>>,
    ) -> Result<Self, SyncError> {
        Self::connect_with_transport_mode(connector, doc, device_id, tuning, transport, None).await
    }

    async fn connect_with_transport_mode(
        connector: Arc<dyn TextConnector>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Option<Arc<dyn RegistryTransport>>,
        admission: Option<RegistryAdmission>,
    ) -> Result<Self, SyncError> {
        Self::connect_with_transport_mode_until(
            connector,
            doc,
            device_id,
            tuning,
            transport,
            admission,
            std::future::pending::<()>(),
        )
        .await
    }

    async fn connect_with_transport_mode_until<F>(
        connector: Arc<dyn TextConnector>,
        doc: Arc<Mutex<RegistryDoc>>,
        device_id: &str,
        tuning: RegistryTuning,
        transport: Option<Arc<dyn RegistryTransport>>,
        admission: Option<RegistryAdmission>,
        stop: F,
    ) -> Result<Self, SyncError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (events, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (nudge_tx, nudge_rx) = mpsc::channel(1);
        let (probe_tx, probe_rx) = mpsc::channel(1);
        let (redial_tx, redial_rx) = mpsc::channel(1);
        let (presence_tx, presence_rx) = mpsc::channel(4);
        let presence = Arc::new(Mutex::new(HashMap::new()));
        let stats = Arc::new(Stats::default());
        let offline = Arc::new(OfflineTasks::default());

        let actor = Actor {
            writer: DocWriter {
                doc: doc.clone(),
                admission,
                retry_flight_reset: Arc::new(AtomicBool::new(false)),
                shutdown: shutdown_tx.clone(),
            },
            device_id: device_id.to_string(),
            connector,
            tuning,
            events: events.clone(),
            shutdown: shutdown_rx,
            nudge_rx,
            probe_rx,
            redial_rx,
            redial: redial_tx.clone(),
            presence_rx,
            presence: presence.clone(),
            stats: stats.clone(),
            transport,
            offline: offline.clone(),
        };
        let mut pending = PendingConnection {
            task: Some(tokio::spawn(actor.run(ready_tx))),
            offline: offline.clone(),
            child: None,
        };

        match pending.ready_or_stop(ready_rx, stop).await {
            Ok(()) => Ok(Self {
                doc,
                events,
                shutdown: shutdown_tx,
                nudge: nudge_tx,
                probe: probe_tx,
                redial: redial_tx,
                presence_out: presence_tx,
                presence,
                stats,
                task: pending.task.take(),
                offline,
            }),
            Err(err) => Err(err),
        }
    }

    pub fn doc(&self) -> &Arc<Mutex<RegistryDoc>> {
        &self.doc
    }

    pub fn events(&self) -> broadcast::Receiver<RegistryEvent> {
        self.events.subscribe()
    }

    /// Local writes were enqueued — push pending batches now.
    pub fn nudge(&self) {
        let _ = self.nudge.try_send(());
    }

    /// Publish this device's presence beat (epoch ms).
    pub fn set_presence(&self, at: i64) {
        let _ = self.presence_out.try_send(at);
    }

    /// Remote devices' live presence beats (entries within the 30s TTL),
    /// device → beat epoch ms.
    pub fn presence(&self) -> HashMap<String, i64> {
        let now = tokio::time::Instant::now();
        lock(&self.presence)
            .iter()
            .filter(|(_, (_, seen))| now.duration_since(*seen) < PRESENCE_TTL)
            .map(|(device, (at, _))| (device.clone(), *at))
            .collect()
    }

    /// Liveness hint: probe the room now (deadline-checked).
    pub fn probe(&self) {
        let _ = self.probe.try_send(());
    }

    /// Escalation: tear the session down and dial a fresh socket.
    pub fn redial(&self) {
        let _ = self.redial.try_send(());
    }

    pub fn stats(&self) -> RoomStatsSnapshot {
        self.stats.snapshot()
    }

    /// Current reconnect posture (next-dial deadline + sticky last failure)
    /// for connection-status UI.
    pub fn reconnect_state(&self) -> ReconnectState {
        ReconnectState {
            retry_at_ms: self
                .stats
                .retry_at_ms
                .load(std::sync::atomic::Ordering::Relaxed),
            last_failure: lock(&self.stats.last_failure).clone(),
        }
    }

    /// Revoke this membership synchronously and return its tasks for an async
    /// drain. No new HTTPS child may start after `offline.stop()`; both existing
    /// tasks are aborted before this returns. An already executing mutation may
    /// finish, so callers must await the returned drain before their final
    /// snapshot. Canceling an in-progress *connection* is a separate path.
    pub fn begin_stop(mut self) -> RegistryDrain {
        self.shutdown.send_replace(true);
        let offline = self.offline.stop();
        if let Some(child) = &offline {
            child.abort();
        }
        let actor = self.task.take();
        if let Some(task) = &actor {
            task.abort();
        }
        RegistryDrain { actor, offline }
    }

    /// Leave cleanly and stop the actor.
    pub async fn shutdown(mut self) {
        let child = self.offline.stop();
        if let Some(child) = &child {
            child.abort();
        }
        let _ = self.shutdown.send(true);
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        if let Some(child) = child {
            let _ = child.await;
        }
    }
}

impl Drop for RegistryClient {
    fn drop(&mut self) {
        let child = self.offline.stop();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(child) = child {
            child.abort();
        }
    }
}

// ── the actor ───────────────────────────────────────────────────────────────

struct Actor {
    writer: DocWriter,
    device_id: String,
    connector: Arc<dyn TextConnector>,
    tuning: RegistryTuning,
    events: broadcast::Sender<RegistryEvent>,
    shutdown: watch::Receiver<bool>,
    nudge_rx: mpsc::Receiver<()>,
    probe_rx: mpsc::Receiver<()>,
    redial_rx: mpsc::Receiver<()>,
    redial: mpsc::Sender<()>,
    presence_rx: mpsc::Receiver<i64>,
    presence: Arc<Mutex<HashMap<String, (i64, tokio::time::Instant)>>>,
    stats: Arc<Stats>,
    /// Plain-HTTPS pull/push (None = socket-only: tests, dev bearers).
    transport: Option<Arc<dyn RegistryTransport>>,
    /// One tracked HTTPS sync in flight at a time; shutdown fences new tasks.
    offline: Arc<OfflineTasks>,
}

enum SessionEnd {
    /// Transport died / probe deadline / requested redial: back off, redial.
    Reconnect,
    /// Shutdown requested: stop the actor.
    Stop,
}

fn admission_end(error: EpochError) -> SessionEnd {
    match error {
        EpochError::Retired => SessionEnd::Stop,
        _ => SessionEnd::Reconnect,
    }
}

/// How a backoff wait ended.
enum Waited {
    Elapsed,
    /// System wake or a sibling dial succeeded: redial NOW on fresh backoff.
    Woke,
    Shutdown,
}

impl Actor {
    async fn run(self, ready: oneshot::Sender<Result<(), SyncError>>) {
        let offline = self.offline.clone();
        self.run_inner(ready).await;
        offline.abort();
    }

    async fn run_inner(mut self, ready: oneshot::Sender<Result<(), SyncError>>) {
        let mut ready = Some(ready);
        let mut backoff = BACKOFF_BASE;
        // Pull-first bootstrap: with an HTTPS transport, the doc syncs in
        // ~1 round trip while the socket spends its 4+ on TLS + upgrade +
        // hello — and construction resolves immediately (local-first: the
        // doc is usable now; keeping it converged is this actor's job, over
        // whichever transport the network permits). Socket-only clients
        // keep the original contract: ready resolves on the first join.
        if self.transport.is_some() {
            if let Some(ready) = ready.take() {
                let _ = ready.send(Ok(()));
            }
            self.spawn_offline_sync();
        }
        // Suspend/resume and sibling-dial successes are EVENTS that end a
        // backoff wait immediately (see room.rs) — without them a recovered
        // network still waited out the full accumulated delay.
        let mut wake = crate::wake::subscribe();
        let mut online = crate::wake::subscribe_online();
        loop {
            if *self.shutdown.borrow() {
                return;
            }
            let attempt = self
                .stats
                .dial_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            let dial = tokio::select! {
                result = tokio::time::timeout(CONNECT_TIMEOUT, self.connector.connect()) => result,
                changed = self.shutdown.changed() => {
                    if changed.is_err() || *self.shutdown.borrow() {
                        return;
                    }
                    continue;
                }
            };
            let pipe = match dial {
                Ok(Ok(pipe)) => pipe,
                Ok(Err(err)) => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Err(err));
                        return; // first join failed: caller owns the retry
                    }
                    tracing::warn!(error = %err, attempt, "registry dial failed; backing off");
                    self.note_failure(format!("dial failed: {err}"));
                    self.spawn_offline_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                    continue;
                }
                Err(_) => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Err(SyncError::WebSocket("connect timeout".into())));
                        return;
                    }
                    tracing::warn!(attempt, "registry dial timed out; backing off");
                    self.note_failure("dial timed out".into());
                    self.spawn_offline_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                    continue;
                }
            };

            let session_started = tokio::time::Instant::now();
            match self.run_session(pipe, &mut ready).await {
                SessionEnd::Stop => return,
                SessionEnd::Reconnect => {
                    if let Err(EpochError::Retired) = self.writer.mark_disconnected() {
                        return;
                    }
                    // Only a session that joined AND stayed healthy for a
                    // while earns a fresh backoff. Reset-on-join alone let a
                    // connect-and-die socket (captive portal, half-broken
                    // NAT) hot-loop at 250ms forever; without any reset, ~7
                    // flaps pinned every future reconnect at the cap for the
                    // life of the client.
                    let joined = self
                        .stats
                        .connected
                        .swap(false, std::sync::atomic::Ordering::Relaxed);
                    self.stats
                        .disconnects
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    self.note_failure("connection dropped".into());
                    let _ = self.events.send(RegistryEvent::Disconnected);
                    if ready.is_some() {
                        // Handshake failed on the very first session.
                        if let Some(ready) = ready.take() {
                            let _ = ready
                                .send(Err(SyncError::Protocol("registry handshake failed".into())));
                        }
                        return;
                    }
                    if joined && session_started.elapsed() >= STABLE_RESET {
                        backoff = BACKOFF_BASE;
                    }
                    // Socket down: sync over HTTPS while we wait — pending
                    // pushes flush and the doc stays fresh at backoff cadence
                    // (≤16s), so a WS-hostile network degrades to polling
                    // instead of silence.
                    self.spawn_offline_sync();
                    match self.wait_backoff(&mut wake, &mut online, backoff).await {
                        Waited::Shutdown => return,
                        Waited::Woke => backoff = BACKOFF_BASE,
                        Waited::Elapsed => backoff = (backoff * 2).min(BACKOFF_CAP),
                    }
                }
            }
        }
    }

    /// Record the failure that started/extended the current outage (sticky
    /// until the next successful join — the UI keeps it visible through the
    /// next attempt instead of flickering back to a bare "connecting").
    fn note_failure(&self, msg: String) {
        *lock(&self.stats.last_failure) = Some(msg);
    }

    /// Sleep out one backoff, cut short by system wake, a sibling dial
    /// success, or shutdown. While the OS reports no network path, the wait
    /// parks on the event buses (with a coarse safety timer) instead of
    /// burning dial attempts that cannot succeed.
    async fn wait_backoff(
        &mut self,
        wake: &mut tokio::sync::broadcast::Receiver<()>,
        online: &mut tokio::sync::broadcast::Receiver<()>,
        wait: Duration,
    ) -> Waited {
        // Drain stale events: only wakes/successes DURING this wait count,
        // or our own last dial would cut every wait to zero.
        while wake.try_recv().is_ok() {}
        while online.try_recv().is_ok() {}
        use std::sync::atomic::Ordering::Relaxed;
        let wait = if crate::wake::path_is_offline() {
            wait.max(OFFLINE_PARK_RECHECK)
        } else {
            wait
        };
        self.stats
            .retry_at_ms
            .store(epoch_ms() + wait.as_millis() as i64, Relaxed);
        let waited = tokio::select! {
            _ = tokio::time::sleep(wait) => Waited::Elapsed,
            _ = wake.recv() => Waited::Woke,
            _ = online.recv() => Waited::Woke,
            _ = self.shutdown.changed() => {
                if *self.shutdown.borrow() {
                    Waited::Shutdown
                } else {
                    Waited::Elapsed
                }
            }
        };
        self.stats.retry_at_ms.store(0, Relaxed);
        waited
    }

    async fn run_session(
        &mut self,
        mut pipe: TextPipe,
        ready: &mut Option<oneshot::Sender<Result<(), SyncError>>>,
    ) -> SessionEnd {
        use std::sync::atomic::Ordering::Relaxed;

        // ── hello / state handshake ─────────────────────────────────────────
        let cursor = {
            let doc = lock(&self.writer.doc);
            let cursor = doc.cursor();
            if cursor == 0 && doc.pending_len() == 0 && doc.generation() == 0 {
                None
            } else {
                Some(cursor)
            }
        };
        let hello = serde_json::to_string(&ClientFrame::Hello {
            cursor,
            device: &self.device_id,
        })
        .expect("hello serializes");
        if pipe.tx.send(hello).await.is_err() {
            return SessionEnd::Reconnect;
        }
        let state = tokio::select! {
            result = tokio::time::timeout(HELLO_DEADLINE, async {
            loop {
                match pipe.rx.recv().await {
                    Some(text) => match serde_json::from_str::<ServerFrame>(&text) {
                        Ok(frame @ ServerFrame::State { .. }) => return Some(frame),
                        Ok(_) => continue, // stale broadcast before our state
                        Err(err) => {
                            tracing::warn!(error = %err, "registry: bad frame during handshake");
                            return None;
                        }
                    },
                    None => None?,
                }
            }
            }) => result,
            changed = self.shutdown.changed() => {
                if changed.is_err() || *self.shutdown.borrow() {
                    return SessionEnd::Stop;
                }
                return SessionEnd::Reconnect;
            }
        };
        let Ok(Some(ServerFrame::State {
            seq,
            full,
            gc_floor,
            rows,
            presence,
        })) = state
        else {
            tracing::warn!("registry: no state frame within deadline");
            return SessionEnd::Reconnect;
        };
        let outcome = match self
            .writer
            .mutate(|doc| doc.apply_state(seq, full, gc_floor, rows))
        {
            Ok(outcome) => outcome,
            Err(error) => return admission_end(error),
        };
        if full {
            self.stats.full_resyncs.fetch_add(1, Relaxed);
        }
        if outcome == StateOutcome::Reseeded {
            tracing::info!("registry: server behind local state; re-seeding");
        }
        {
            let now = tokio::time::Instant::now();
            let mut map = lock(&self.presence);
            for (device, at) in presence {
                map.insert(device, (at, now));
            }
        }
        self.stats.connected.store(true, Relaxed);
        self.stats.synced.store(true, Relaxed);
        self.stats.last_pushed_ms.store(epoch_ms(), Relaxed);
        self.stats.retry_at_ms.store(0, Relaxed);
        *lock(&self.stats.last_failure) = None;
        if ready.is_none() {
            self.stats.rejoins.fetch_add(1, Relaxed);
        }
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        let _ = self.events.send(RegistryEvent::Connected);
        let _ = self.events.send(RegistryEvent::Applied);

        // Anything pending (offline writes, reseeds, migration) pushes now.
        match self.push_pending(&mut pipe).await {
            Ok(true) => {}
            Ok(false) => return SessionEnd::Reconnect,
            Err(error) => return admission_end(error),
        }

        // ── steady state ────────────────────────────────────────────────────
        let mut last_frame = tokio::time::Instant::now();
        let mut probe_deadline: Option<tokio::time::Instant> = None;
        loop {
            let quiet_probe_at = last_frame + self.tuning.probe_quiet;
            let deadline_at = probe_deadline
                .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
            tokio::select! {
                frame = pipe.rx.recv() => {
                    let Some(text) = frame else {
                        return SessionEnd::Reconnect;
                    };
                    last_frame = tokio::time::Instant::now();
                    probe_deadline = None;
                    match self.handle_frame(&text) {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Reconnect,
                        Err(error) => return admission_end(error),
                    }
                }
                _ = self.nudge_rx.recv() => {
                    match self.push_pending(&mut pipe).await {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Reconnect,
                        Err(error) => return admission_end(error),
                    }
                }
                at = self.presence_rx.recv() => {
                    if let Some(at) = at {
                        let frame = serde_json::to_string(&ClientFrame::Presence { at })
                            .expect("presence serializes");
                        if pipe.tx.send(frame).await.is_err() {
                            return SessionEnd::Reconnect;
                        }
                    }
                }
                _ = self.probe_rx.recv() => {
                    if !self.send_probe(&mut pipe, &mut probe_deadline).await {
                        return SessionEnd::Reconnect;
                    }
                }
                _ = self.redial_rx.recv() => {
                    tracing::info!("registry: redial requested");
                    return SessionEnd::Reconnect;
                }
                _ = tokio::time::sleep_until(quiet_probe_at) => {
                    if !self.send_probe(&mut pipe, &mut probe_deadline).await {
                        return SessionEnd::Reconnect;
                    }
                    // Don't re-arm the quiet timer against the same silence.
                    last_frame = tokio::time::Instant::now();
                }
                _ = tokio::time::sleep_until(deadline_at) => {
                    tracing::warn!("registry: probe unanswered past deadline; redialing");
                    return SessionEnd::Reconnect;
                }
                _ = self.shutdown.changed() => {
                    if *self.shutdown.borrow() {
                        return SessionEnd::Stop;
                    }
                }
            }
        }
    }

    async fn send_probe(
        &self,
        pipe: &mut TextPipe,
        probe_deadline: &mut Option<tokio::time::Instant>,
    ) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        self.stats.probes.fetch_add(1, Relaxed);
        let frame = serde_json::to_string(&ClientFrame::Probe).expect("probe serializes");
        if pipe.tx.send(frame).await.is_err() {
            return false;
        }
        if probe_deadline.is_none() {
            *probe_deadline = Some(tokio::time::Instant::now() + PROBE_DEADLINE);
        }
        true
    }

    /// One HTTPS sync cycle off the actor's critical path: flush pending op
    /// batches (POST — LWW replays are no-ops), then pull the delta (GET) and
    /// apply it exactly as a state frame. Single-flight; failures are logged
    /// and retried on the next backoff cycle.
    fn spawn_offline_sync(&self) {
        let Some(transport) = self.transport.clone() else {
            return;
        };
        let mut state = lock(&self.offline.0);
        if state.stopping
            || state
                .handle
                .as_ref()
                .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        state.handle.take(); // completed child, if any
        let writer = self.writer.clone();
        let events = self.events.clone();
        let presence = self.presence.clone();
        let stats = self.stats.clone();
        let redial = self.redial.clone();
        state.handle = Some(tokio::spawn(async move {
            offline_sync_once(writer, transport, events, presence, stats, redial).await;
        }));
    }

    async fn push_pending(&self, pipe: &mut TextPipe) -> Result<bool, EpochError> {
        let batches = self.writer.take_pushable()?;
        for batch in batches {
            let frame = serde_json::to_string(&ClientFrame::Push {
                batch: &batch.batch,
                ops: &batch.ops,
            })
            .expect("push serializes");
            if pipe.tx.send(frame).await.is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Apply one inbound protocol frame. False = protocol breakdown, redial.
    fn handle_frame(&self, text: &str) -> Result<bool, EpochError> {
        use std::sync::atomic::Ordering::Relaxed;
        let frame = match serde_json::from_str::<ServerFrame>(text) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::warn!(error = %err, "registry: unparseable frame");
                return Ok(false);
            }
        };
        match frame {
            ServerFrame::Rows { seq, rows } => {
                let contiguous = self.writer.mutate(|doc| doc.apply_rows(seq, rows))?;
                self.stats.last_pushed_ms.store(epoch_ms(), Relaxed);
                let _ = self.events.send(RegistryEvent::Applied);
                if !contiguous {
                    // Frames between the cursor and this batch were missed;
                    // the cursor held. Reconnect — the fresh hello pulls the
                    // whole gap from the true cursor.
                    tracing::warn!(seq, "registry: broadcast seq gap; resyncing");
                    return Ok(false);
                }
            }
            ServerFrame::Ack { batch, seq, .. } => {
                if let Err(error) = self.writer.mutate(|doc| doc.ack_batch(&batch, seq)) {
                    self.writer.defer_flight_reset();
                    return Err(error);
                }
                self.stats.last_ack_ms.store(epoch_ms(), Relaxed);
                let _ = self.events.send(RegistryEvent::Applied);
            }
            ServerFrame::Presence { device, at } => {
                lock(&self.presence).insert(device, (at, tokio::time::Instant::now()));
                let _ = self.events.send(RegistryEvent::Presence);
            }
            ServerFrame::ProbeOk { .. } => {
                self.stats.last_pushed_ms.store(epoch_ms(), Relaxed);
            }
            ServerFrame::State {
                seq,
                full,
                gc_floor,
                rows,
                presence,
            } => {
                // Servers only send state as a hello answer, but applying a
                // late duplicate is harmless and simpler than special-casing.
                self.writer
                    .mutate(|doc| doc.apply_state(seq, full, gc_floor, rows))?;
                let now = tokio::time::Instant::now();
                let mut map = lock(&self.presence);
                for (device, at) in presence {
                    map.insert(device, (at, now));
                }
                let _ = self.events.send(RegistryEvent::Applied);
            }
            ServerFrame::Error { code, message } => {
                self.stats.rejected.fetch_add(1, Relaxed);
                tracing::warn!(code, message, "registry: server rejected a frame");
            }
        }
        Ok(true)
    }
}

async fn offline_sync_once(
    writer: DocWriter,
    transport: Arc<dyn RegistryTransport>,
    events: broadcast::Sender<RegistryEvent>,
    presence: Arc<Mutex<HashMap<String, (i64, tokio::time::Instant)>>>,
    stats: Arc<Stats>,
    redial: mpsc::Sender<()>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let batches = match writer.take_pushable() {
        Ok(batches) => batches,
        Err(error) => {
            request_redial_on_busy(error, &redial);
            return;
        }
    };
    let mut push_failed = false;
    for batch in &batches {
        let body = serde_json::json!({ "batch": batch.batch, "ops": batch.ops }).to_string();
        match transport.push(body).await {
            Ok(ack) => {
                let parsed = serde_json::from_str::<serde_json::Value>(&ack)
                    .ok()
                    .and_then(|value| {
                        Some((value["batch"].as_str()?.to_owned(), value["seq"].as_u64()?))
                    });
                match parsed {
                    Some((id, seq)) => {
                        if let Err(error) = writer.mutate(|doc| doc.ack_batch(&id, seq)) {
                            writer.defer_flight_reset();
                            request_redial_on_busy(error, &redial);
                            return;
                        }
                        stats.last_ack_ms.store(epoch_ms(), Relaxed);
                        let _ = events.send(RegistryEvent::Applied);
                    }
                    None => {
                        tracing::warn!("registry: http push ack unreadable; will retry");
                        push_failed = true;
                        break;
                    }
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry: http push failed; will retry");
                push_failed = true;
                break;
            }
        }
    }
    if push_failed && let Err(error) = writer.mark_disconnected() {
        request_redial_on_busy(error, &redial);
        return;
    }
    let since = lock(&writer.doc).cursor();
    match transport.fetch(since).await {
        Ok(body) => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct PullBody {
                seq: u64,
                full: bool,
                gc_floor: u64,
                rows: Vec<RegistryRow>,
                #[serde(default)]
                presence: HashMap<String, i64>,
            }
            match serde_json::from_str::<PullBody>(&body) {
                Ok(pull) => {
                    if let Err(error) = writer.mutate(|doc| {
                        doc.apply_state(pull.seq, pull.full, pull.gc_floor, pull.rows)
                    }) {
                        request_redial_on_busy(error, &redial);
                        return; // Cursor unchanged; the redial re-fetches.
                    }
                    let now = tokio::time::Instant::now();
                    let mut map = lock(&presence);
                    for (device, at) in pull.presence {
                        map.insert(device, (at, now));
                    }
                    drop(map);
                    stats.synced.store(true, Relaxed);
                    stats.last_pushed_ms.store(epoch_ms(), Relaxed);
                    let _ = events.send(RegistryEvent::Applied);
                }
                Err(err) => tracing::warn!(error = %err, "registry: http pull body unparseable"),
            }
        }
        Err(err) => tracing::warn!(error = %err, "registry: http pull failed; will retry"),
    }
}

fn request_redial_on_busy(error: EpochError, redial: &mpsc::Sender<()>) {
    if error == EpochError::Busy {
        let _ = redial.try_send(());
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::write_epoch::WriteEpoch;
    use std::sync::atomic::AtomicUsize;

    struct ChannelConnector {
        pipes: Arc<tokio::sync::Mutex<mpsc::Receiver<TextPipe>>>,
    }

    impl TextConnector for ChannelConnector {
        fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>> {
            let pipes = self.pipes.clone();
            Box::pin(async move { pipes.lock().await.recv().await.ok_or(SyncError::Closed) })
        }
    }

    fn connector() -> (Arc<ChannelConnector>, mpsc::Sender<TextPipe>) {
        let (tx, rx) = mpsc::channel(4);
        (
            Arc::new(ChannelConnector {
                pipes: Arc::new(tokio::sync::Mutex::new(rx)),
            }),
            tx,
        )
    }

    fn pipe() -> (TextPipe, mpsc::Receiver<String>, mpsc::Sender<String>) {
        let (out_tx, out_rx) = mpsc::channel(8);
        let (in_tx, in_rx) = mpsc::channel(8);
        (
            TextPipe {
                tx: out_tx,
                rx: in_rx,
            },
            out_rx,
            in_tx,
        )
    }

    fn admission(epoch: WriteEpoch) -> RegistryAdmission {
        Arc::new(move || {
            epoch
                .try_admit()
                .map(|ticket| Box::new(ticket) as RegistryWritePermit)
        })
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if condition() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_rows_wait_for_reconnect_after_snapshot_without_advancing_cursor() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
        let (connector, pipes) = connector();
        let (first_pipe, mut first_out, first_in) = pipe();
        pipes.send(first_pipe).await.unwrap();
        let connecting = tokio::spawn({
            let doc = doc.clone();
            let gate = admission(epoch.clone());
            async move {
                RegistryClient::connect_with_transport_mode(
                    connector,
                    doc,
                    "device-a",
                    RegistryTuning::default(),
                    None,
                    Some(gate),
                )
                .await
            }
        });
        let hello = first_out.recv().await.unwrap();
        assert!(hello.contains("\"t\":\"hello\""));
        first_in
            .send(r#"{"t":"state","seq":0,"full":false,"rows":[]}"#.into())
            .await
            .unwrap();
        let client = connecting.await.unwrap().unwrap();
        let mut events = client.events();
        let before = {
            let doc = lock(&doc);
            (doc.cursor(), doc.generation())
        };
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        first_in
            .send(r#"{"t":"rows","seq":1,"rows":[]}"#.into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while events.recv().await.unwrap() != RegistryEvent::Disconnected {}
        })
        .await
        .unwrap();
        assert_eq!(
            {
                let doc = lock(&doc);
                (doc.cursor(), doc.generation())
            },
            before
        );
        drop(snapshot);

        let (second_pipe, mut second_out, second_in) = pipe();
        pipes.send(second_pipe).await.unwrap();
        let hello = tokio::time::timeout(Duration::from_secs(2), second_out.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(hello.contains("\"cursor\":0"), "{hello}");
        second_in
            .send(r#"{"t":"state","seq":1,"full":false,"rows":[]}"#.into())
            .await
            .unwrap();
        wait_until(|| lock(&doc).cursor() == 1).await;
        client.shutdown().await;
    }

    struct AckTransport {
        started: Mutex<Option<oneshot::Sender<()>>>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
        pushes: Arc<AtomicUsize>,
    }

    impl RegistryTransport for AckTransport {
        fn push(&self, body: String) -> BoxFuture<'static, Result<String, SyncError>> {
            let started = lock(&self.started).take();
            let release = lock(&self.release).take();
            let pushes = self.pushes.clone();
            Box::pin(async move {
                pushes.fetch_add(1, Ordering::Relaxed);
                if let Some(started) = started {
                    let _ = started.send(());
                }
                if let Some(release) = release {
                    let _ = release.await;
                }
                let value: serde_json::Value = serde_json::from_str(&body).unwrap();
                Ok(serde_json::json!({"batch":value["batch"],"seq":1}).to_string())
            })
        }

        fn fetch(&self, _since: u64) -> BoxFuture<'static, Result<String, SyncError>> {
            Box::pin(async { Ok(r#"{"seq":0,"full":false,"rows":[]}"#.into()) })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_https_ack_denial_retries_same_pending_batch_after_thaw() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
        lock(&doc)
            .upsert_device(&luna_proto::Device {
                id: "device-a".into(),
                name: "Synthetic".into(),
                platform: "test".into(),
                last_seen_at: None,
                created_at: Some(chrono::Utc::now()),
                version: None,
                capabilities: Vec::new(),
            })
            .unwrap();
        let (shutdown, _) = watch::channel(false);
        let writer = DocWriter {
            doc: doc.clone(),
            admission: Some(admission(epoch.clone())),
            retry_flight_reset: Arc::new(AtomicBool::new(false)),
            shutdown,
        };
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let pushes = Arc::new(AtomicUsize::new(0));
        let transport: Arc<dyn RegistryTransport> = Arc::new(AckTransport {
            started: Mutex::new(Some(started_tx)),
            release: Mutex::new(Some(release_rx)),
            pushes: pushes.clone(),
        });
        let (events, _) = broadcast::channel(8);
        let presence = Arc::new(Mutex::new(HashMap::new()));
        let stats = Arc::new(Stats::default());
        let (redial, mut redial_rx) = mpsc::channel(1);
        let first = tokio::spawn(offline_sync_once(
            writer.clone(),
            transport.clone(),
            events.clone(),
            presence.clone(),
            stats.clone(),
            redial.clone(),
        ));
        started_rx.await.unwrap();
        let pending_before_denied_ack = lock(&doc).to_bytes().unwrap();
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        release_tx.send(()).unwrap();
        first.await.unwrap();
        assert_eq!(lock(&doc).pending_len(), 1);
        assert_eq!(lock(&doc).cursor(), 0);
        assert_eq!(lock(&doc).to_bytes().unwrap(), pending_before_denied_ack);
        assert!(
            redial_rx.try_recv().is_ok(),
            "Busy HTTPS ACK must request replay"
        );
        drop(snapshot);

        offline_sync_once(writer, transport, events, presence, stats, redial).await;
        assert_eq!(pushes.load(Ordering::Relaxed), 2);
        assert_eq!(lock(&doc).pending_len(), 0);
    }

    struct DelayedFetch {
        started: Mutex<Option<oneshot::Sender<()>>>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
        fetches: Arc<AtomicUsize>,
    }

    impl RegistryTransport for DelayedFetch {
        fn push(&self, _body: String) -> BoxFuture<'static, Result<String, SyncError>> {
            Box::pin(async { Err(SyncError::Closed) })
        }

        fn fetch(&self, _since: u64) -> BoxFuture<'static, Result<String, SyncError>> {
            let started = lock(&self.started).take();
            let release = lock(&self.release).take();
            let fetches = self.fetches.clone();
            Box::pin(async move {
                fetches.fetch_add(1, Ordering::Relaxed);
                if let Some(started) = started {
                    let _ = started.send(());
                }
                if let Some(release) = release {
                    let _ = release.await;
                }
                Ok(r#"{"seq":1,"full":false,"gcFloor":0,"rows":[]}"#.into())
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guarded_https_pull_denial_requests_refetch_from_unchanged_cursor() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
        let (shutdown, _) = watch::channel(false);
        let writer = DocWriter {
            doc: doc.clone(),
            admission: Some(admission(epoch.clone())),
            retry_flight_reset: Arc::new(AtomicBool::new(false)),
            shutdown,
        };
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let fetches = Arc::new(AtomicUsize::new(0));
        let transport: Arc<dyn RegistryTransport> = Arc::new(DelayedFetch {
            started: Mutex::new(Some(started_tx)),
            release: Mutex::new(Some(release_rx)),
            fetches: fetches.clone(),
        });
        let (events, _) = broadcast::channel(8);
        let presence = Arc::new(Mutex::new(HashMap::new()));
        let stats = Arc::new(Stats::default());
        let (redial, mut redial_rx) = mpsc::channel(1);
        let first = tokio::spawn(offline_sync_once(
            writer.clone(),
            transport.clone(),
            events.clone(),
            presence.clone(),
            stats.clone(),
            redial.clone(),
        ));
        started_rx.await.unwrap();
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        release_tx.send(()).unwrap();
        first.await.unwrap();
        assert_eq!(lock(&doc).cursor(), 0);
        assert!(redial_rx.try_recv().is_ok());
        drop(snapshot);

        offline_sync_once(writer, transport, events, presence, stats, redial).await;
        assert_eq!(fetches.load(Ordering::Relaxed), 2);
        assert_eq!(lock(&doc).cursor(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admitted_doc_mutation_keeps_freeze_quiescing_until_it_finishes() {
        use crate::write_epoch::Phase;

        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let (shutdown, _) = watch::channel(false);
        let writer = DocWriter {
            doc: Arc::new(Mutex::new(RegistryDoc::new("device-a"))),
            admission: Some(admission(epoch.clone())),
            retry_flight_reset: Arc::new(AtomicBool::new(false)),
            shutdown,
        };
        let doc = writer.doc.clone();
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mutating = tokio::task::spawn_blocking(move || {
            writer
                .mutate(|doc| {
                    let _ = entered_tx.send(());
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let _ = doc.apply_rows(1, Vec::new());
                })
                .unwrap();
        });
        entered_rx.await.unwrap();
        let freezing = tokio::spawn({
            let epoch = epoch.clone();
            async move { epoch.freeze(Duration::from_secs(3)).await }
        });
        wait_until(|| epoch.phase() == Phase::Quiescing).await;
        assert!(!freezing.is_finished());
        release_tx.send(()).unwrap();
        mutating.await.unwrap();
        let snapshot = freezing.await.unwrap().unwrap().into_snapshot().unwrap();
        assert_eq!(lock(&doc).cursor(), 1);
        drop(snapshot);
    }

    struct FetchDrop(Arc<AtomicBool>);

    impl Drop for FetchDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    struct HangingFetch {
        started: Mutex<Option<oneshot::Sender<()>>>,
        dropped: Arc<AtomicBool>,
    }

    impl RegistryTransport for HangingFetch {
        fn push(&self, _body: String) -> BoxFuture<'static, Result<String, SyncError>> {
            Box::pin(async { Err(SyncError::Closed) })
        }

        fn fetch(&self, _since: u64) -> BoxFuture<'static, Result<String, SyncError>> {
            let started = lock(&self.started).take();
            let dropped = self.dropped.clone();
            Box::pin(async move {
                let _guard = FetchDrop(dropped);
                if let Some(started) = started {
                    let _ = started.send(());
                }
                std::future::pending().await
            })
        }
    }

    struct HangingDial {
        started: Mutex<Option<oneshot::Sender<()>>>,
        dropped: Arc<AtomicBool>,
    }

    impl TextConnector for HangingDial {
        fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>> {
            let started = lock(&self.started).take();
            let dropped = self.dropped.clone();
            Box::pin(async move {
                let _guard = FetchDrop(dropped);
                if let Some(started) = started {
                    let _ = started.send(());
                }
                std::future::pending().await
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn begin_stop_revokes_fake_dial_and_https_child_before_async_join() {
        for join in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
            let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
            let (dial_started_tx, dial_started_rx) = oneshot::channel();
            let (fetch_started_tx, fetch_started_rx) = oneshot::channel();
            let dial_dropped = Arc::new(AtomicBool::new(false));
            let fetch_dropped = Arc::new(AtomicBool::new(false));
            let client = RegistryClient::connect_with_transport_mode(
                Arc::new(HangingDial {
                    started: Mutex::new(Some(dial_started_tx)),
                    dropped: dial_dropped.clone(),
                }),
                doc.clone(),
                "device-a",
                RegistryTuning::default(),
                Some(Arc::new(HangingFetch {
                    started: Mutex::new(Some(fetch_started_tx)),
                    dropped: fetch_dropped.clone(),
                })),
                Some(admission(epoch)),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(2), dial_started_rx)
                .await
                .unwrap()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), fetch_started_rx)
                .await
                .unwrap()
                .unwrap();
            let shutdown = client.shutdown.subscribe();
            let offline = client.offline.clone();

            let drain = client.begin_stop();
            assert!(*shutdown.borrow());
            assert!(lock(&offline.0).stopping);
            assert!(drain.actor.is_some());
            assert!(drain.offline.is_some());
            if join {
                tokio::time::timeout(Duration::from_secs(2), drain.join())
                    .await
                    .unwrap();
            } else {
                drop(drain);
            }
            wait_until(|| {
                dial_dropped.load(Ordering::Acquire) && fetch_dropped.load(Ordering::Acquire)
            })
            .await;
            assert_eq!(lock(&doc).cursor(), 0);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_unused_registry_drain_aborts_both_handles() {
        let actor_dropped = Arc::new(AtomicBool::new(false));
        let offline_dropped = Arc::new(AtomicBool::new(false));
        let (actor_started_tx, actor_started_rx) = oneshot::channel();
        let (offline_started_tx, offline_started_rx) = oneshot::channel();
        let actor = tokio::spawn({
            let dropped = actor_dropped.clone();
            async move {
                let _guard = FetchDrop(dropped);
                let _ = actor_started_tx.send(());
                std::future::pending::<()>().await;
            }
        });
        let offline = tokio::spawn({
            let dropped = offline_dropped.clone();
            async move {
                let _guard = FetchDrop(dropped);
                let _ = offline_started_tx.send(());
                std::future::pending::<()>().await;
            }
        });
        tokio::time::timeout(Duration::from_secs(2), actor_started_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), offline_started_rx)
            .await
            .unwrap()
            .unwrap();

        drop(RegistryDrain {
            actor: Some(actor),
            offline: Some(offline),
        });
        wait_until(|| {
            actor_dropped.load(Ordering::Acquire) && offline_dropped.load(Ordering::Acquire)
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actor_exit_abort_keeps_https_handle_available_for_join() {
        let offline = OfflineTasks::default();
        let dropped = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = oneshot::channel();
        let child = tokio::spawn({
            let dropped = dropped.clone();
            async move {
                let _guard = FetchDrop(dropped);
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            }
        });
        lock(&offline.0).handle = Some(child);
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .unwrap()
            .unwrap();

        offline.abort();
        let child = offline.stop().expect("actor exit retained child handle");
        let _ = child.await;
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_and_drop_cancel_https_child_and_fake_dial() {
        for graceful in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
            let (connector, _pipes) = connector(); // no pipe: actor is parked in dial
            let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
            let (started_tx, started_rx) = oneshot::channel();
            let dropped = Arc::new(AtomicBool::new(false));
            let client = RegistryClient::connect_with_transport_mode(
                connector,
                doc.clone(),
                "device-a",
                RegistryTuning::default(),
                Some(Arc::new(HangingFetch {
                    started: Mutex::new(Some(started_tx)),
                    dropped: dropped.clone(),
                })),
                Some(admission(epoch)),
            )
            .await
            .unwrap();
            started_rx.await.unwrap();
            if graceful {
                tokio::time::timeout(Duration::from_secs(2), client.shutdown())
                    .await
                    .unwrap();
            } else {
                drop(client);
            }
            wait_until(|| dropped.load(Ordering::Acquire)).await;
            assert_eq!(lock(&doc).cursor(), 0);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retired_admission_stops_guarded_actor_without_applying_frame() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
        let (connector, pipes) = connector();
        let (socket, mut outgoing, incoming) = pipe();
        pipes.send(socket).await.unwrap();
        let connecting = tokio::spawn({
            let doc = doc.clone();
            let gate = admission(epoch.clone());
            async move {
                RegistryClient::connect_with_transport_mode(
                    connector,
                    doc,
                    "device-a",
                    RegistryTuning::default(),
                    None,
                    Some(gate),
                )
                .await
            }
        });
        outgoing.recv().await.unwrap();
        incoming
            .send(r#"{"t":"state","seq":0,"full":false,"rows":[]}"#.into())
            .await
            .unwrap();
        let client = connecting.await.unwrap().unwrap();
        let before = {
            let doc = lock(&doc);
            (doc.cursor(), doc.generation())
        };
        epoch.retire();
        incoming
            .send(r#"{"t":"rows","seq":1,"rows":[]}"#.into())
            .await
            .unwrap();
        wait_until(|| client.task.as_ref().unwrap().is_finished()).await;
        assert_eq!(
            {
                let doc = lock(&doc);
                (doc.cursor(), doc.generation())
            },
            before
        );
        assert!(*client.shutdown.borrow());
        client.shutdown().await;
    }

    struct PendingConnector {
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl TextConnector for PendingConnector {
        fn connect(&self) -> BoxFuture<'static, Result<TextPipe, SyncError>> {
            let started = self.started.clone();
            let dropped = self.dropped.clone();
            Box::pin(async move {
                let _guard = FetchDrop(dropped);
                started.store(true, Ordering::Release);
                std::future::pending().await
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canceled_guarded_constructor_aborts_its_actor() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let connector = Arc::new(PendingConnector {
            started: started.clone(),
            dropped: dropped.clone(),
        });
        let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
        let connecting = tokio::spawn({
            let doc = doc.clone();
            async move {
                RegistryClient::connect_with_transport_mode(
                    connector,
                    doc,
                    "device-a",
                    RegistryTuning::default(),
                    None,
                    Some(admission(epoch)),
                )
                .await
            }
        });
        wait_until(|| started.load(Ordering::Acquire)).await;
        connecting.abort();
        let _ = connecting.await;
        wait_until(|| dropped.load(Ordering::Acquire)).await;
        assert_eq!(lock(&doc).cursor(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn signaled_stop_awaits_pending_guarded_constructor_actor() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let connector = Arc::new(PendingConnector {
            started: started.clone(),
            dropped: dropped.clone(),
        });
        let doc = Arc::new(Mutex::new(RegistryDoc::new("device-a")));
        let (stop_tx, stop_rx) = oneshot::channel();
        let connecting = tokio::spawn({
            let doc = doc.clone();
            async move {
                RegistryClient::connect_with_transport_mode_until(
                    connector,
                    doc,
                    "device-a",
                    RegistryTuning::default(),
                    None,
                    Some(admission(epoch)),
                    async move {
                        let _ = stop_rx.await;
                    },
                )
                .await
            }
        });
        wait_until(|| started.load(Ordering::Acquire)).await;
        stop_tx.send(()).unwrap();
        let result = connecting.await.unwrap();
        assert!(matches!(result, Err(SyncError::Closed)));
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(lock(&doc).cursor(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stop_wins_ready_tie_and_awaits_pending_actor_and_https_child() {
        let offline = Arc::new(OfflineTasks::default());
        let actor_dropped = Arc::new(AtomicBool::new(false));
        let child_dropped = Arc::new(AtomicBool::new(false));
        let (actor_started_tx, actor_started_rx) = oneshot::channel();
        let (child_started_tx, child_started_rx) = oneshot::channel();
        let actor = tokio::spawn({
            let dropped = actor_dropped.clone();
            async move {
                let _guard = FetchDrop(dropped);
                let _ = actor_started_tx.send(());
                std::future::pending::<()>().await;
            }
        });
        let child = tokio::spawn({
            let dropped = child_dropped.clone();
            async move {
                let _guard = FetchDrop(dropped);
                let _ = child_started_tx.send(());
                std::future::pending::<()>().await;
            }
        });
        lock(&offline.0).handle = Some(child);
        let mut pending = PendingConnection {
            task: Some(actor),
            offline,
            child: None,
        };
        tokio::time::timeout(Duration::from_secs(2), actor_started_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), child_started_rx)
            .await
            .unwrap()
            .unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        ready_tx.send(Ok(())).unwrap();

        let result = pending
            .ready_or_stop(ready_rx, std::future::ready(()))
            .await;
        assert!(matches!(result, Err(SyncError::Closed)));
        assert!(actor_dropped.load(Ordering::Acquire));
        assert!(child_dropped.load(Ordering::Acquire));
    }
}

#[cfg(any(test, feature = "mock-server"))]
pub mod mock_server;
