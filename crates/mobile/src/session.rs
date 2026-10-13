//! The SDK session facade (IOS-00) and its connection runtime (RM-03): one
//! paired Mac's typed operations through the shared `luna-sdk` client, over
//! sockets the platform owns.
//!
//! The split is the one RM-00 proved. Swift performs every network operation
//! with `URLSession` and evaluates the TLS pin; this core decides what each
//! result means and runs the public SDK over the frames Swift carries. There
//! is no second client: frames go through `luna_rpc::RpcClient` and every
//! operation through `luna_sdk::Client`, exactly as the desktop app and the
//! CLI call the engine.
//!
//! Opening a socket takes four steps:
//! 1. the pinned descriptor must name the saved host, protocol and the
//!    catalog this SDK speaks (before any credential is used);
//! 2. the saved bearer buys one single-use socket ticket;
//! 3. the platform opens the pinned socket with that ticket;
//! 4. one typed SDK read (`engineInfo`) must answer as the same host.
//!
//! This is the single connection owner. It has two lifetimes:
//! - [`RemoteSession::new`] (IOS-00) opens one socket and never reconnects:
//!   once closed it stays closed, and the caller creates a new session.
//! - [`RemoteSession::following`] (RM-03) is the runtime. After a lost socket
//!   it reconnects with bounded, jittered backoff; it suspends and resumes
//!   with the app and waits while the platform reports no network. Every
//!   reconnect repeats all four steps, so a stale socket, a cached row or a
//!   public descriptor never counts as readiness. While open it follows the
//!   Mac's task registry and a bounded set of tasks into a host-keyed
//!   [`luna_sdk::ProjectionStore`], independently of any open view, and
//!   resumes each from its cursor (or takes the Mac's snapshot repair) on
//!   the next socket. Each verified socket is a new generation: replies and
//!   projection updates from an older one are refused. Revoked or expired
//!   access, a wrong host and an incompatible Mac close it for good.
//!
//! Threading: every method is callable from any thread. Platform callbacks
//! may arrive on any thread, synchronously or late; replies for anything but
//! the current request or socket are ignored. No lock is held while foreign
//! code runs. The listener is told *that* something changed, from whichever
//! thread changed it, at most once until the platform reads
//! [`RemoteSession::state`]; the platform reads state and projections on its
//! own actor, so notifications arriving out of order cannot reorder states
//! and a burst of updates costs one read.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use luna_proto::remote_control::{
    DESCRIPTOR_PATH, HostTicket, RemoteTarget, SOCKET_PATH, SOCKET_TICKET_PATH,
};
use luna_proto::remote_projection::{MAX_ID_BYTES, REMOTE_PROJECTIONS_V1};
use luna_proto::{CommandReceipt, EngineInfo, TaskCommandSnapshot};
use luna_rpc::RpcClient;
use luna_rpc::remote_auth::SocketTicket;
use luna_sdk::{HostKey, ProjectionError, ProjectionStore, SdkError, TaskCommandUpdate};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

use crate::follow::{RemoteTaskProjection, RemoteTasksProjection, task_projection, tasks_record};
use crate::pairing::{EnrollmentRequest, SavedMac, now, validate_saved};
use crate::probe::{
    Endpoint, MAX_DESCRIPTOR_BYTES, RejectReason, RouteObservation, TransportFailure, parse_ticket,
    verify_descriptor,
};
use crate::runtime::{Cancellation, run, runtime};

/// Every SDK call over the socket is bounded; a host that stops answering is
/// `timeout`, never a hung UI.
const CALL_DEADLINE: Duration = Duration::from_secs(15);
const MAX_TICKET_REPLY_BYTES: u32 = 16 * 1024;
/// The gateway's own WebSocket message bound (RM-01).
const MAX_MESSAGE_BYTES: u32 = 1024 * 1024;
const MAX_TICKET_BYTES: usize = 256;
const MAX_TASK_ID_BYTES: usize = 256;
/// Frames waiting between the platform socket and the RPC client. A full
/// inbound queue makes `receive` wait, which stops the platform reading.
const FRAME_QUEUE: usize = 256;
/// Watch changes waiting for `next`. A consumer that stops reading stalls
/// the SDK watch, whose engine stream is then dropped (CORE-05's bound).
const WATCH_QUEUE: usize = 64;
/// The remote guard's refusal prefix (`luna_rpc::remote_auth`).
const DENIED_PREFIX: &str = "remote authorization denied: ";
/// Tasks the runtime follows at once, besides the registry (most recently
/// followed kept). Each holds one of the gateway's 32 in-flight slots.
pub const MAX_FOLLOWED_TASKS: usize = 4;
/// Resubscriptions of one projection on one socket before it gives up and
/// reports the problem (a reconnect starts over).
const MAX_FOLLOW_RETRIES: u32 = 3;
/// The first reconnect after a lost socket, then doubling to the cap.
const FIRST_RETRY: Duration = Duration::from_millis(250);
const MAX_RETRY: Duration = Duration::from_secs(30);

type SdkClient = luna_sdk::Client<RpcClient>;
type Updates = mpsc::Receiver<Result<TaskCommandChange, RemoteSessionError>>;

/// What one session's socket must look like to the platform.
#[derive(Clone, uniffi::Record)]
pub struct SocketOpenRequest {
    pub socket_id: u64,
    /// `wss://host[:port]/remote/v1/ws` on the verified origin.
    pub url: String,
    /// The only host name the TLS challenge may carry.
    pub host: String,
    /// Lowercase hex SHA-256 of the SubjectPublicKeyInfo the server must
    /// present.
    pub spki_sha256: String,
    /// `Ticket <single-use ticket>`, sent only as the upgrade's
    /// `Authorization` header. Never log it.
    pub authorization: String,
    /// Refuse (and close on) any larger message.
    pub max_message_bytes: u32,
}

/// The platform half of a session: pinned HTTPS requests and pinned
/// WebSockets, one open at a time. Each request is reported once, through
/// [`RemoteSession::request_completed`] or [`RemoteSession::request_failed`];
/// each socket through [`RemoteSession::socket_opened`], then
/// [`RemoteSession::receive`] for each text message in order, then
/// [`RemoteSession::socket_closed`]. Calls may come from any thread.
#[uniffi::export(with_foreign)]
pub trait SocketTransport: Send + Sync {
    fn send_request(&self, request: EnrollmentRequest);
    /// Open a socket. A runtime session opens another after the previous one
    /// ended or was closed with [`SocketTransport::close_socket`].
    fn open_socket(&self, request: SocketOpenRequest);
    /// Send one text message on the open socket, after every earlier one.
    fn send_frame(&self, socket_id: u64, frame: String);
    /// Close this one socket (RM-03: the session will open another). Nothing
    /// needs to be reported for it afterwards. Idempotent.
    fn close_socket(&self, socket_id: u64);
    /// Stop everything for this session: cancel any request and close the
    /// socket. Idempotent; nothing needs to be reported afterwards.
    fn close(&self);
}

/// Told that [`RemoteSession::state`] or a projection changed. Called on
/// whichever thread changed it: hop to the platform's actor, then read the
/// state there. Not called again until the state has been read.
#[uniffi::export(with_foreign)]
pub trait RemoteSessionListener: Send + Sync {
    fn on_change(&self);
}

/// The verified host behind an open socket.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteHostInfo {
    pub device_id: String,
    pub environment_id: String,
    /// `local`, `synced` or `development`.
    pub workspace_scope: String,
    pub profile_id: Option<String>,
    pub capabilities: Vec<String>,
    /// The direct protocol the descriptor named (`remote-direct-v1`).
    pub direct_protocol: String,
    /// The operation catalog the host serves.
    pub catalog_version: u16,
    /// The operation contract this core's SDK speaks.
    pub api_version: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SessionCloseReason {
    /// Closed or released on this phone.
    Local,
    /// The connect was cancelled before the socket was verified.
    Cancelled,
    /// The Mac or the network ended an open socket.
    Remote,
    /// The saved access was revoked or has expired: pair again.
    RePair,
    /// The host, its version or its identity was refused.
    Rejected,
    /// Anything else that stopped the connect.
    Failed,
}

// A plain FFI value: UniFFI cannot pass a boxed variant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum RemoteSessionState {
    Idle,
    Connecting,
    Open {
        host: RemoteHostInfo,
        /// The route of the ticket request on the same pinned origin.
        route: Option<RouteObservation>,
    },
    /// RM-03: the socket was lost or an attempt failed; the session retries
    /// after a bounded backoff. `attempt` counts failed attempts since the
    /// last open socket.
    Reconnecting {
        attempt: u32,
        detail: String,
    },
    /// RM-03: backgrounded on this phone; `resume` reconnects.
    Suspended,
    /// RM-03: the platform reports no network; reconnects when it returns.
    WaitingForNetwork,
    Closed {
        reason: SessionCloseReason,
        detail: String,
    },
}

/// Why an operation did not complete. A refusal by the Mac's remote guard is
/// [`RemoteSessionError::Denied`]; the session itself stays open.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum RemoteSessionError {
    #[error("invalid saved access: {detail}")]
    InvalidSession { detail: String },
    #[error("{detail}")]
    InvalidRequest { detail: String },
    #[error("the session is already connecting or open")]
    Busy,
    #[error("the session is not open")]
    NotOpen,
    #[error("cancelled")]
    Cancelled,
    #[error("the session is closed: {detail}")]
    Closed { detail: String },
    #[error("{detail}")]
    Offline { detail: String },
    #[error("{detail}")]
    Timeout { detail: String },
    #[error("{detail}")]
    TrustRejected { detail: String },
    #[error("{detail}")]
    RePair { detail: String },
    #[error("{detail}")]
    Denied { detail: String },
    #[error("{detail}")]
    Incompatible { detail: String },
    #[error("{detail}")]
    IdentityMismatch { detail: String },
    #[error("{detail}")]
    Unsupported { detail: String },
    #[error("{detail}")]
    InvalidResponse { detail: String },
    #[error("{detail}")]
    Engine { detail: String },
}

/// The runtime helpers name the error by its short in-crate name.
pub(crate) type SessionError = RemoteSessionError;

impl RemoteSessionError {
    fn close_reason(&self) -> SessionCloseReason {
        match self {
            Self::Cancelled => SessionCloseReason::Cancelled,
            Self::RePair { .. } => SessionCloseReason::RePair,
            Self::TrustRejected { .. }
            | Self::IdentityMismatch { .. }
            | Self::Incompatible { .. }
            | Self::Denied { .. } => SessionCloseReason::Rejected,
            Self::Closed { .. } => SessionCloseReason::Local,
            _ => SessionCloseReason::Failed,
        }
    }

    /// A runtime session stops retrying after one of these: the access, the
    /// host or its version is wrong, and another attempt cannot change that.
    fn ends_runtime(&self) -> bool {
        matches!(
            self,
            Self::RePair { .. }
                | Self::TrustRejected { .. }
                | Self::IdentityMismatch { .. }
                | Self::Incompatible { .. }
                | Self::Denied { .. }
                | Self::Unsupported { .. }
                | Self::InvalidSession { .. }
                | Self::InvalidRequest { .. }
        )
    }
}

/// A task's command position: its revision and durable sequence.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TaskCommandState {
    pub task_id: String,
    pub task_revision: u64,
    pub task_sequence: u64,
    pub binding_generation: Option<u32>,
    pub active_turn_id: Option<String>,
}

/// One command receipt as the phone shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TaskCommandEntry {
    pub command_id: String,
    pub intent: String,
    /// The receipt status's wire name (`applied`, `pending`, `conflict`, ...).
    pub status: String,
    pub reason_code: Option<String>,
    /// Its durable task sequence; absent while it has no durable position.
    pub task_sequence: Option<u64>,
}

/// One update of a task-command watch, in the SDK's delivery order: a
/// snapshot first, then each event once and in sequence.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum TaskCommandChange {
    Snapshot {
        state: TaskCommandState,
        events: Vec<TaskCommandEntry>,
    },
    Event {
        entry: TaskCommandEntry,
        state: TaskCommandState,
    },
    /// A receipt with no durable position yet; the position did not move.
    Progress { entry: TaskCommandEntry },
    /// History after the position is gone: replace local state with this.
    Reload { state: TaskCommandState },
}

/// One open task-command watch. Cancelling it (or releasing it) cancels the
/// engine's stream; the session stays usable.
#[derive(uniffi::Object)]
pub struct TaskCommandWatchHandle {
    updates: tokio::sync::Mutex<Updates>,
    task: AbortHandle,
}

#[uniffi::export]
impl TaskCommandWatchHandle {
    /// The next change. `None` once the watch was cancelled, its socket or
    /// session closed or the engine ended the stream: open a new watch,
    /// which starts from a fresh snapshot.
    pub async fn next(&self) -> Result<Option<TaskCommandChange>, RemoteSessionError> {
        let mut updates = self.updates.lock().await;
        updates.recv().await.transpose()
    }

    /// End the watch now. A pending [`TaskCommandWatchHandle::next`] returns
    /// `None`.
    pub fn cancel(&self) {
        self.task.abort();
    }
}

impl Drop for TaskCommandWatchHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One saved Mac's SDK session. See the module documentation.
#[derive(uniffi::Object)]
pub struct RemoteSession {
    core: Arc<Core>,
}

#[uniffi::export]
impl RemoteSession {
    /// IOS-00: one socket. Validate saved access with the same rules as
    /// restore; nothing touches the network until [`RemoteSession::connect`].
    #[uniffi::constructor]
    pub fn new(
        mac: SavedMac,
        endpoint_index: u32,
        transport: Arc<dyn SocketTransport>,
        listener: Option<Arc<dyn RemoteSessionListener>>,
    ) -> Result<Arc<Self>, RemoteSessionError> {
        Ok(Arc::new(Self {
            core: Arc::new(Core::build(mac, endpoint_index, transport, listener, None)?),
        }))
    }

    /// RM-03: the reconnecting runtime that follows the Mac's tasks. `cache`
    /// is what [`RemoteSession::cache_bytes`] returned on an earlier run for
    /// this same Mac; it is restored as `cached` and confirmed or repaired
    /// on the first socket. A cache for any other host, profile or version is
    /// ignored (see [`RemoteSession::cache_note`]).
    #[uniffi::constructor]
    pub fn following(
        mac: SavedMac,
        endpoint_index: u32,
        transport: Arc<dyn SocketTransport>,
        listener: Option<Arc<dyn RemoteSessionListener>>,
        cache: Option<Vec<u8>>,
    ) -> Result<Arc<Self>, RemoteSessionError> {
        Ok(Arc::new(Self {
            core: Arc::new(Core::build(
                mac,
                endpoint_index,
                transport,
                listener,
                Some(cache),
            )?),
        }))
    }

    /// Open and verify the socket.
    ///
    /// IOS-00 session: once; a failed, cancelled or closed session stays
    /// closed, and a second call is `busy`.
    ///
    /// Runtime session: starts the runtime and returns the first socket's
    /// host, or the first attempt's error while the runtime keeps retrying
    /// (or the reason it closed). Calling it again while open returns the
    /// open host; cancelling the call leaves the runtime running.
    pub async fn connect(
        &self,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<RemoteHostInfo, RemoteSessionError> {
        let core = self.core.clone();
        if core.follows() {
            return run(cancel, async move { core.start_runtime().await }).await;
        }
        run(cancel, async move { core.connect().await }).await
    }

    /// Read the host's engine identity again through the SDK. A cancelled
    /// read leaves the session open; its late reply is dropped.
    pub async fn engine_info(
        &self,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<RemoteHostInfo, RemoteSessionError> {
        let core = self.core.clone();
        run(cancel, async move { core.read_engine().await }).await
    }

    /// Watch a task's command receipts from its current snapshot. Returns
    /// once the engine's stream is live.
    pub async fn watch_task_commands(
        &self,
        task_id: String,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<Arc<TaskCommandWatchHandle>, RemoteSessionError> {
        let core = self.core.clone();
        run(cancel, async move { core.watch(task_id).await }).await
    }

    /// Close on this phone: pending operations end with `closed`, watches
    /// end, and the platform is told to close.
    pub fn close(&self) {
        self.core
            .terminate(SessionCloseReason::Local, "Closed on this phone".into());
    }

    /// The current state. Reading it acknowledges the last change
    /// notification: the next change notifies again.
    pub fn state(&self) -> RemoteSessionState {
        self.core.pending.store(false, Ordering::SeqCst);
        self.core.state()
    }

    /// RM-03: the app went to the background. Close the socket (accepted
    /// work on the Mac continues); projections turn `stale`. Keep the
    /// session, save [`RemoteSession::cache_bytes`], and call
    /// [`RemoteSession::resume`] in the foreground.
    pub fn suspend(&self) {
        self.core.suspend();
    }

    /// RM-03: the app is in the foreground again: reconnect now (with full
    /// verification) and resume every projection from its cursor.
    pub fn resume(&self) {
        self.core.resume();
    }

    /// RM-03: the platform's network path changed. Without a network the
    /// socket is closed and the session waits; when one returns it
    /// reconnects at once. An open socket survives a path change until its
    /// keepalive or the Mac ends it.
    pub fn network_changed(&self, available: bool) {
        self.core.network_changed(available);
    }

    /// RM-03: follow this task's detail (its row, command position and a
    /// bounded transcript tail) from now on, whether or not a view shows
    /// it. At most [`MAX_FOLLOWED_TASKS`] are followed; following another
    /// stops the least recently followed one (its cached detail stays).
    pub fn follow_task(&self, task_id: String) -> Result<(), RemoteSessionError> {
        if task_id.is_empty() || task_id.len() > MAX_ID_BYTES {
            return Err(RemoteSessionError::InvalidRequest {
                detail: format!("a task id is 1-{MAX_ID_BYTES} bytes"),
            });
        }
        self.core.follow_task(task_id)
    }

    /// RM-03: stop following a task; its cached detail stays readable.
    pub fn unfollow_task(&self, task_id: String) {
        self.core.unfollow_task(&task_id);
    }

    /// RM-03: the followed tasks, least recently followed first.
    pub fn followed_tasks(&self) -> Vec<String> {
        self.core
            .lock()
            .follow
            .as_ref()
            .map(|follow| follow.followed.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// RM-03: the Mac's projects and tasks as last confirmed, with their
    /// freshness; `None` before the first snapshot (and without a cache).
    pub fn remote_tasks(&self) -> Option<RemoteTasksProjection> {
        let view = self.core.lock().follow.as_ref()?.store.registry()?;
        Some(tasks_record(&view))
    }

    /// RM-03: one task's detail as last confirmed (followed or cached).
    pub fn remote_task(&self, task_id: String) -> Option<RemoteTaskProjection> {
        let view = self.core.lock().follow.as_ref()?.store.task(&task_id)?;
        Some(task_projection(&view))
    }

    /// RM-03: grows whenever a projection or its freshness changed, so the
    /// platform re-reads projections only when this moved.
    pub fn projection_revision(&self) -> u64 {
        self.core
            .lock()
            .follow
            .as_ref()
            .map_or(0, |follow| follow.revision)
    }

    /// RM-03: why the Mac's projections are not being followed (an older
    /// Mac, refused access or repeated failures), if they are not.
    pub fn projection_issue(&self) -> Option<String> {
        self.core.lock().follow.as_ref()?.issue.clone()
    }

    /// RM-03: why the cache given to [`RemoteSession::following`] was not
    /// restored, if it was not.
    pub fn cache_note(&self) -> Option<String> {
        self.core.lock().follow.as_ref()?.cache_note.clone()
    }

    /// RM-03: the projections to keep for the next run, bounded. Store them
    /// in protected, non-synchronized storage scoped to this saved Mac, and
    /// delete them when the Mac is forgotten. Empty for an IOS-00 session.
    pub fn cache_bytes(&self) -> Vec<u8> {
        let inner = self.core.lock();
        inner
            .follow
            .as_ref()
            .map(|follow| follow.store.encode(now_ms()))
            .unwrap_or_default()
    }

    pub fn request_completed(
        &self,
        request_id: u64,
        status: u16,
        body: Vec<u8>,
        route: RouteObservation,
    ) {
        if let Some(reply) = self.core.take_request(request_id) {
            let _ = reply.send(Reply::Http {
                status,
                body,
                route,
            });
        }
    }

    pub fn request_failed(&self, request_id: u64, failure: TransportFailure) {
        if let Some(reply) = self.core.take_request(request_id) {
            let _ = reply.send(Reply::Failed(failure));
        }
    }

    pub fn socket_opened(&self, socket_id: u64) {
        let opened = {
            let mut inner = self.core.lock();
            inner
                .socket
                .as_mut()
                .filter(|socket| socket.id == socket_id)
                .and_then(|socket| socket.opened.take())
        };
        if let Some(opened) = opened {
            let _ = opened.send(Ok(()));
        }
    }

    /// One text message from the open socket. Waits while the core's frame
    /// queue is full, so the platform reads no faster than the core consumes.
    pub async fn receive(&self, socket_id: u64, frame: String) {
        if frame.len() > MAX_MESSAGE_BYTES as usize {
            let detail = format!("The Mac sent a message over {MAX_MESSAGE_BYTES} bytes");
            if self.core.follows() {
                self.core.socket_lost(socket_id, detail, true);
            } else {
                self.core.terminate(SessionCloseReason::Failed, detail);
            }
            return;
        }
        let Some(inbound) = self.core.inbound(socket_id) else {
            return;
        };
        let _ = inbound.send(frame).await;
    }

    pub fn socket_closed(&self, socket_id: u64, failure: TransportFailure) {
        let opening = {
            let mut inner = self.core.lock();
            match inner.socket.as_mut() {
                Some(socket) if socket.id == socket_id => Some(socket.opened.take()),
                _ => None,
            }
        };
        match opening {
            // Not opened yet: the connect reports why.
            Some(Some(opening)) => {
                let _ = opening.send(Err(failure));
            }
            Some(None) if self.core.follows() => {
                self.core
                    .socket_lost(socket_id, transport_error(failure).to_string(), false);
            }
            Some(None) => self.core.terminate(
                SessionCloseReason::Remote,
                transport_error(failure).to_string(),
            ),
            None => {}
        }
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        // Released by the platform: nothing may keep its socket open.
        self.core
            .terminate(SessionCloseReason::Local, "Released on this phone".into());
    }
}

enum Phase {
    Idle,
    Connecting,
    Open(RemoteHostInfo),
    Reconnecting { attempt: u32, detail: String },
    Suspended,
    WaitingForNetwork,
    Closed(SessionCloseReason, String),
}

enum Reply {
    Http {
        status: u16,
        body: Vec<u8>,
        route: RouteObservation,
    },
    Failed(TransportFailure),
}

struct Socket {
    id: u64,
    /// Present until the platform reports the socket open (or closed).
    opened: Option<oneshot::Sender<Result<(), TransportFailure>>>,
    inbound: mpsc::Sender<String>,
}

struct Inner {
    phase: Phase,
    next_id: u64,
    request: Option<(u64, oneshot::Sender<Reply>)>,
    socket: Option<Socket>,
    client: Option<Arc<SdkClient>>,
    route: Option<RouteObservation>,
    /// The socket's tasks: the outbound pump, every watch and projection
    /// follower. All abort when the socket ends or the session closes.
    tasks: Vec<AbortHandle>,
    /// RM-03 runtime state; `None` for an IOS-00 single-socket session.
    follow: Option<Follow>,
}

impl Inner {
    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// The runtime's state, guarded with the rest of [`Inner`].
struct Follow {
    store: ProjectionStore,
    /// The current socket generation. It moves when a socket opens and when
    /// it ends, so a late update from any earlier socket is refused.
    epoch: u64,
    /// Failed attempts since the last open socket.
    attempt: u32,
    /// What the platform last said about the network.
    network: bool,
    supervisor: Option<AbortHandle>,
    /// Followed tasks, least recently followed first.
    followed: VecDeque<String>,
    /// The current socket's task followers.
    followers: Vec<(String, AbortHandle)>,
    last_error: Option<RemoteSessionError>,
    cache_note: Option<String>,
    issue: Option<String>,
    revision: u64,
}

struct Core {
    host: HostTicket,
    endpoint: Endpoint,
    token: String,
    transport: Arc<dyn SocketTransport>,
    listener: Option<Arc<dyn RemoteSessionListener>>,
    closed: watch::Sender<bool>,
    /// Bumped on every state change; the runtime's supervisor and connect
    /// wait on it.
    wake: watch::Sender<u64>,
    /// A change was signalled and the platform has not read the state since.
    pending: AtomicBool,
    inner: Mutex<Inner>,
}

/// What the runtime's supervisor does next.
enum Step {
    Stop,
    Wait,
    Connect,
    Retry(Duration),
}

/// Why a follower stops applying its stream.
enum Refusal {
    /// The socket (or session) it belonged to is gone.
    Stop,
    /// The stream does not follow the held state: resubscribe for a snapshot.
    Resnapshot,
}

impl Core {
    fn build(
        mac: SavedMac,
        endpoint_index: u32,
        transport: Arc<dyn SocketTransport>,
        listener: Option<Arc<dyn RemoteSessionListener>>,
        follow: Option<Option<Vec<u8>>>,
    ) -> Result<Self, RemoteSessionError> {
        let host = validate_saved(&mac).map_err(|error| RemoteSessionError::InvalidSession {
            detail: error.to_string(),
        })?;
        if mac.expires_at_ms <= now() {
            return Err(RemoteSessionError::RePair {
                detail: "Access has expired; pair again on the Mac".into(),
            });
        }
        let (_, endpoints) =
            parse_ticket(&mac.host_ticket).map_err(|error| RemoteSessionError::InvalidSession {
                detail: error.to_string(),
            })?;
        let endpoint = endpoints
            .into_iter()
            .nth(endpoint_index as usize)
            .ok_or_else(|| RemoteSessionError::InvalidSession {
                detail: format!("the saved Mac has no endpoint {endpoint_index}"),
            })?;
        let follow = follow.map(|cache| {
            let key = HostKey {
                device_id: host.target.device_id.clone(),
                environment_id: host.target.environment_id.clone(),
                workspace_scope: host.target.workspace_scope,
            };
            let (store, cache_note) = match cache {
                None => (ProjectionStore::new(key), None),
                Some(bytes) => match ProjectionStore::decode(&bytes, &key) {
                    Ok(store) => (store, None),
                    Err(error) => (ProjectionStore::new(key), Some(error.to_string())),
                },
            };
            Follow {
                store,
                epoch: 0,
                attempt: 0,
                network: true,
                supervisor: None,
                followed: VecDeque::new(),
                followers: Vec::new(),
                last_error: None,
                cache_note,
                issue: None,
                revision: 1,
            }
        });
        Ok(Self {
            host,
            endpoint,
            token: mac.session_token,
            transport,
            listener,
            closed: watch::Sender::new(false),
            wake: watch::Sender::new(0),
            pending: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                phase: Phase::Idle,
                next_id: 1,
                request: None,
                socket: None,
                client: None,
                route: None,
                tasks: Vec::new(),
                follow,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn follows(&self) -> bool {
        self.lock().follow.is_some()
    }

    /// A state change: wake the runtime's waiters and tell the platform.
    fn notify(&self) {
        self.wake
            .send_modify(|count| *count = count.wrapping_add(1));
        self.signal();
    }

    /// Tell the platform something changed, once until it reads the state.
    fn signal(&self) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(listener) = &self.listener {
            listener.on_change();
        }
    }

    fn state(&self) -> RemoteSessionState {
        let inner = self.lock();
        match &inner.phase {
            Phase::Idle => RemoteSessionState::Idle,
            Phase::Connecting => RemoteSessionState::Connecting,
            Phase::Open(host) => RemoteSessionState::Open {
                host: host.clone(),
                route: inner.route.clone(),
            },
            Phase::Reconnecting { attempt, detail } => RemoteSessionState::Reconnecting {
                attempt: *attempt,
                detail: detail.clone(),
            },
            Phase::Suspended => RemoteSessionState::Suspended,
            Phase::WaitingForNetwork => RemoteSessionState::WaitingForNetwork,
            Phase::Closed(reason, detail) => RemoteSessionState::Closed {
                reason: *reason,
                detail: detail.clone(),
            },
        }
    }

    fn closed_error(&self) -> RemoteSessionError {
        match &self.lock().phase {
            Phase::Closed(_, detail) => RemoteSessionError::Closed {
                detail: detail.clone(),
            },
            _ => RemoteSessionError::Closed {
                detail: "the session ended".into(),
            },
        }
    }

    /// An SDK failure, or `closed` when the session closed underneath it.
    fn sdk_error(&self, error: SdkError) -> RemoteSessionError {
        if matches!(self.lock().phase, Phase::Closed(..)) {
            return self.closed_error();
        }
        from_sdk(error)
    }

    /// Close once; the first reason wins. Safe from any thread, including
    /// from inside a platform callback.
    fn terminate(&self, reason: SessionCloseReason, detail: String) {
        let (socket, client, request, tasks) = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            if matches!(inner.phase, Phase::Closed(..)) {
                return;
            }
            inner.phase = Phase::Closed(reason, detail);
            let mut tasks = std::mem::take(&mut inner.tasks);
            if let Some(follow) = inner.follow.as_mut() {
                follow.epoch += 1;
                follow.store.end_epoch();
                follow.followers.clear();
                follow.revision += 1;
                tasks.extend(follow.supervisor.take());
            }
            (
                inner.socket.take(),
                inner.client.take(),
                inner.request.take(),
                tasks,
            )
        };
        self.closed.send_replace(true);
        for task in &tasks {
            task.abort();
        }
        // Dropping the inbound sender ends the RPC reader, which fails every
        // pending call with `closed` even while a caller still holds the
        // client.
        drop((socket, client, request));
        self.transport.close();
        self.notify();
    }

    /// RM-03: end the current socket (if any) and move to `next`, keeping
    /// the session. Projections turn stale and their followers stop.
    fn drop_socket(&self, next: Phase, reset_attempts: bool) {
        let (socket, client, request, tasks) = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            if matches!(inner.phase, Phase::Closed(..)) {
                return;
            }
            inner.phase = next;
            if let Some(follow) = inner.follow.as_mut() {
                follow.epoch += 1;
                follow.store.end_epoch();
                follow.followers.clear();
                follow.revision += 1;
                if reset_attempts {
                    follow.attempt = 0;
                }
            }
            inner.route = None;
            (
                inner.socket.take(),
                inner.client.take(),
                inner.request.take(),
                std::mem::take(&mut inner.tasks),
            )
        };
        for task in &tasks {
            task.abort();
        }
        let socket_id = socket.as_ref().map(|socket| socket.id);
        drop((socket, client, request));
        if let Some(socket_id) = socket_id {
            self.transport.close_socket(socket_id);
        }
        self.notify();
    }

    /// RM-03: the open (or opening) socket `socket_id` ended: reconnect.
    /// `failed` counts it as a failed attempt (a protocol fault).
    fn socket_lost(&self, socket_id: u64, detail: String, failed: bool) {
        let attempt = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let current = inner
                .socket
                .as_ref()
                .is_some_and(|socket| socket.id == socket_id);
            let live = matches!(inner.phase, Phase::Open(_) | Phase::Connecting);
            let Some(follow) = inner.follow.as_mut() else {
                return;
            };
            if !current || !live {
                return;
            }
            if failed || matches!(inner.phase, Phase::Connecting) {
                follow.attempt = follow.attempt.saturating_add(1);
            }
            follow.attempt
        };
        self.drop_socket(Phase::Reconnecting { attempt, detail }, false);
    }

    fn suspend(&self) {
        let active = {
            let inner = self.lock();
            inner.follow.is_some()
                && !matches!(
                    inner.phase,
                    Phase::Idle | Phase::Suspended | Phase::Closed(..)
                )
        };
        if active {
            self.drop_socket(Phase::Suspended, true);
        }
    }

    fn resume(&self) {
        let changed = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let Some(follow) = inner.follow.as_mut() else {
                return;
            };
            if !matches!(
                inner.phase,
                Phase::Suspended | Phase::Reconnecting { .. } | Phase::WaitingForNetwork
            ) {
                return;
            }
            follow.attempt = 0;
            inner.phase = if follow.network {
                Phase::Connecting
            } else {
                Phase::WaitingForNetwork
            };
            true
        };
        if changed {
            self.notify();
        }
    }

    fn network_changed(&self, available: bool) {
        let (reconnect, lost) = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let Some(follow) = inner.follow.as_mut() else {
                return;
            };
            follow.network = available;
            let reconnect = available
                && matches!(
                    inner.phase,
                    Phase::WaitingForNetwork | Phase::Reconnecting { .. }
                );
            let lost = !available
                && matches!(
                    inner.phase,
                    Phase::Open(_) | Phase::Connecting | Phase::Reconnecting { .. }
                );
            if reconnect {
                follow.attempt = 0;
                inner.phase = Phase::Connecting;
            }
            (reconnect, lost)
        };
        if reconnect {
            self.notify();
        }
        if lost {
            self.drop_socket(Phase::WaitingForNetwork, true);
        }
    }

    fn track(&self, task: AbortHandle) {
        let mut inner = self.lock();
        if matches!(inner.phase, Phase::Closed(..)) {
            task.abort();
            return;
        }
        inner.tasks.retain(|task| !task.is_finished());
        inner.tasks.push(task);
    }

    fn take_request(&self, request_id: u64) -> Option<oneshot::Sender<Reply>> {
        let mut inner = self.lock();
        match inner.request.take() {
            Some((id, reply)) if id == request_id => Some(reply),
            other => {
                inner.request = other;
                None
            }
        }
    }

    fn inbound(&self, socket_id: u64) -> Option<mpsc::Sender<String>> {
        let inner = self.lock();
        inner
            .socket
            .as_ref()
            .filter(|socket| socket.id == socket_id && socket.opened.is_none())
            .map(|socket| socket.inbound.clone())
    }

    fn open_client(&self) -> Result<(Arc<SdkClient>, RemoteHostInfo), RemoteSessionError> {
        let inner = self.lock();
        match (&inner.phase, &inner.client) {
            (Phase::Open(host), Some(client)) => Ok((client.clone(), host.clone())),
            (Phase::Closed(_, detail), _) => Err(RemoteSessionError::Closed {
                detail: detail.clone(),
            }),
            _ => Err(RemoteSessionError::NotOpen),
        }
    }

    /// Wait for `reply` unless the session closes first.
    async fn until_closed<T>(&self, reply: oneshot::Receiver<T>) -> Result<T, RemoteSessionError> {
        let mut closed = self.closed.subscribe();
        tokio::select! {
            biased;
            _ = async { let _ = closed.wait_for(|value| *value).await; } => Err(self.closed_error()),
            result = reply => result.map_err(|_| self.closed_error()),
        }
    }

    /// IOS-00: one connect.
    async fn connect(self: Arc<Self>) -> Result<RemoteHostInfo, RemoteSessionError> {
        let refused = {
            let mut inner = self.lock();
            let refused = match &inner.phase {
                Phase::Idle => None,
                Phase::Closed(_, detail) => Some(RemoteSessionError::Closed {
                    detail: detail.clone(),
                }),
                _ => Some(RemoteSessionError::Busy),
            };
            if refused.is_none() {
                inner.phase = Phase::Connecting;
            }
            refused
        };
        if let Some(refused) = refused {
            return Err(refused);
        }
        self.notify();
        // A cancelled connect drops this future: the guard closes the
        // session so a half-open socket never outlives it.
        let mut guard = CloseUnlessSettled {
            core: self.clone(),
            settled: false,
        };
        let result = self.handshake().await;
        guard.settled = true;
        if let Err(error) = &result {
            self.terminate(error.close_reason(), error.to_string());
        }
        result
    }

    /// RM-03: start the runtime (once) and wait for its first outcome.
    async fn start_runtime(self: Arc<Self>) -> Result<RemoteHostInfo, RemoteSessionError> {
        let mut wake = self.wake.subscribe();
        let start = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            if let Phase::Closed(_, detail) = &inner.phase {
                return Err(RemoteSessionError::Closed {
                    detail: detail.clone(),
                });
            }
            if matches!(inner.phase, Phase::Idle) {
                let network = inner.follow.as_ref().is_none_or(|follow| follow.network);
                inner.phase = if network {
                    Phase::Connecting
                } else {
                    Phase::WaitingForNetwork
                };
                true
            } else {
                false
            }
        };
        if start {
            let supervisor = runtime().spawn(self.clone().supervise());
            {
                let mut guard = self.lock();
                let inner = &mut *guard;
                let closed = matches!(inner.phase, Phase::Closed(..));
                match inner.follow.as_mut() {
                    Some(follow) if !closed => {
                        follow.supervisor = Some(supervisor.abort_handle());
                    }
                    _ => supervisor.abort(),
                }
            }
            self.notify();
        }
        loop {
            {
                let inner = self.lock();
                let last_error = inner
                    .follow
                    .as_ref()
                    .and_then(|follow| follow.last_error.clone());
                match &inner.phase {
                    Phase::Open(host) => return Ok(host.clone()),
                    Phase::Closed(_, detail) => {
                        return Err(last_error.unwrap_or_else(|| RemoteSessionError::Closed {
                            detail: detail.clone(),
                        }));
                    }
                    Phase::Reconnecting { detail, .. } => {
                        return Err(last_error.unwrap_or_else(|| RemoteSessionError::Offline {
                            detail: detail.clone(),
                        }));
                    }
                    Phase::Suspended => return Err(RemoteSessionError::NotOpen),
                    Phase::WaitingForNetwork => {
                        return Err(RemoteSessionError::Offline {
                            detail: "This phone has no network connection".into(),
                        });
                    }
                    Phase::Idle | Phase::Connecting => {}
                }
            }
            if wake.changed().await.is_err() {
                return Err(self.closed_error());
            }
        }
    }

    /// RM-03: the runtime loop. It owns every attempt after the first
    /// `connect` and stops when the session closes.
    async fn supervise(self: Arc<Self>) {
        let mut wake = self.wake.subscribe();
        loop {
            match self.next_step() {
                Step::Stop => return,
                Step::Wait => {
                    if wake.changed().await.is_err() {
                        return;
                    }
                }
                Step::Retry(delay) => {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => self.retry_now(),
                        changed = wake.changed() => if changed.is_err() { return },
                    }
                }
                Step::Connect => {
                    let outcome = {
                        let handshake = self.handshake();
                        tokio::pin!(handshake);
                        loop {
                            tokio::select! {
                                biased;
                                result = &mut handshake => break Some(result),
                                changed = wake.changed() => {
                                    if changed.is_err() {
                                        return;
                                    }
                                    // Suspended, offline or closed: abandon
                                    // the attempt; the phase says why.
                                    if !matches!(self.lock().phase, Phase::Connecting) {
                                        break None;
                                    }
                                }
                            }
                        }
                    };
                    match outcome {
                        None => {}
                        Some(Ok(_)) => self.opened(),
                        Some(Err(error)) => self.attempt_failed(error),
                    }
                }
            }
        }
    }

    fn next_step(&self) -> Step {
        let inner = self.lock();
        let attempt = inner.follow.as_ref().map_or(0, |follow| follow.attempt);
        match inner.phase {
            Phase::Closed(..) => Step::Stop,
            Phase::Connecting => Step::Connect,
            Phase::Reconnecting { .. } => Step::Retry(backoff(attempt)),
            Phase::Idle | Phase::Open(_) | Phase::Suspended | Phase::WaitingForNetwork => {
                Step::Wait
            }
        }
    }

    fn retry_now(&self) {
        let retry = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let network = inner.follow.as_ref().is_some_and(|follow| follow.network);
            if network && matches!(inner.phase, Phase::Reconnecting { .. }) {
                inner.phase = Phase::Connecting;
                true
            } else {
                false
            }
        };
        if retry {
            self.notify();
        }
    }

    /// RM-03: a failed attempt either ends the runtime or waits to retry.
    fn attempt_failed(&self, error: RemoteSessionError) {
        if error.ends_runtime() {
            {
                let mut inner = self.lock();
                if let Some(follow) = inner.follow.as_mut() {
                    follow.last_error = Some(error.clone());
                }
            }
            self.terminate(error.close_reason(), error.to_string());
            return;
        }
        let attempt = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            if !matches!(inner.phase, Phase::Connecting) {
                return;
            }
            let Some(follow) = inner.follow.as_mut() else {
                return;
            };
            follow.attempt = follow.attempt.saturating_add(1);
            follow.last_error = Some(error.clone());
            follow.attempt
        };
        self.drop_socket(
            Phase::Reconnecting {
                attempt,
                detail: error.to_string(),
            },
            false,
        );
    }

    /// RM-03: a verified socket is open: start its generation and follow
    /// the registry and every followed task on it.
    fn opened(self: &Arc<Self>) {
        let started = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let (Phase::Open(host), Some(client), Some(follow)) =
                (&inner.phase, inner.client.clone(), inner.follow.as_mut())
            else {
                return;
            };
            follow.epoch += 1;
            follow.attempt = 0;
            follow.last_error = None;
            follow.store.begin_epoch(follow.epoch);
            follow.revision += 1;
            if host
                .capabilities
                .iter()
                .any(|capability| capability == REMOTE_PROJECTIONS_V1)
            {
                follow.issue = None;
                Some((client, follow.epoch, follow.followed.clone()))
            } else {
                follow.issue = Some(
                    "This Mac's LUNA does not share task lists yet; update LUNA on the Mac".into(),
                );
                None
            }
        };
        self.signal();
        let Some((client, epoch, followed)) = started else {
            return;
        };
        let registry = runtime().spawn(run_registry_follower(self.clone(), client.clone(), epoch));
        self.track(registry.abort_handle());
        for task_id in followed {
            self.spawn_task_follower(task_id, client.clone(), epoch);
        }
    }

    fn spawn_task_follower(self: &Arc<Self>, task_id: String, client: Arc<SdkClient>, epoch: u64) {
        let follower = runtime().spawn(run_task_follower(
            self.clone(),
            client,
            epoch,
            task_id.clone(),
        ));
        let handle = follower.abort_handle();
        {
            let mut guard = self.lock();
            let inner = &mut *guard;
            match inner.follow.as_mut() {
                Some(follow) if follow.epoch == epoch => {
                    follow.followers.push((task_id, handle.clone()));
                }
                _ => {
                    handle.abort();
                    return;
                }
            }
        }
        self.track(handle);
    }

    fn follow_task(self: &Arc<Self>, task_id: String) -> Result<(), RemoteSessionError> {
        let (spawn, stopped) = {
            let mut guard = self.lock();
            let inner = &mut *guard;
            let Some(follow) = inner.follow.as_mut() else {
                return Err(RemoteSessionError::InvalidRequest {
                    detail: "this session does not follow tasks".into(),
                });
            };
            if let Some(position) = follow.followed.iter().position(|id| *id == task_id) {
                // Already followed: it becomes the most recent.
                if let Some(id) = follow.followed.remove(position) {
                    follow.followed.push_back(id);
                }
                return Ok(());
            }
            follow.followed.push_back(task_id.clone());
            let mut stopped = Vec::new();
            while follow.followed.len() > MAX_FOLLOWED_TASKS {
                if let Some(oldest) = follow.followed.pop_front() {
                    follow.followers.retain(|(id, handle)| {
                        if *id == oldest {
                            stopped.push(handle.clone());
                            false
                        } else {
                            true
                        }
                    });
                    follow.store.release_task(&oldest);
                }
            }
            follow.revision += 1;
            let spawn = match (&inner.phase, inner.client.clone()) {
                (Phase::Open(_), Some(client)) if follow.issue.is_none() => {
                    Some((client, follow.epoch))
                }
                _ => None,
            };
            (spawn, stopped)
        };
        for handle in stopped {
            handle.abort();
        }
        if let Some((client, epoch)) = spawn {
            self.spawn_task_follower(task_id, client, epoch);
        }
        self.signal();
        Ok(())
    }

    fn unfollow_task(&self, task_id: &str) {
        let stopped = {
            let mut inner = self.lock();
            let Some(follow) = inner.follow.as_mut() else {
                return;
            };
            follow.followed.retain(|id| id != task_id);
            follow.store.release_task(task_id);
            let mut stopped = Vec::new();
            follow.followers.retain(|(id, handle)| {
                if id == task_id {
                    stopped.push(handle.clone());
                    false
                } else {
                    true
                }
            });
            follow.revision += 1;
            stopped
        };
        for handle in stopped {
            handle.abort();
        }
        self.signal();
    }

    fn is_epoch(&self, epoch: u64) -> bool {
        self.lock()
            .follow
            .as_ref()
            .is_some_and(|follow| follow.epoch == epoch)
    }

    fn set_issue(&self, epoch: u64, issue: String) {
        {
            let mut inner = self.lock();
            let Some(follow) = inner.follow.as_mut() else {
                return;
            };
            if follow.epoch != epoch {
                return;
            }
            follow.issue = Some(issue);
            follow.revision += 1;
        }
        self.signal();
    }

    /// Apply one registry item from socket `epoch`.
    fn apply_registry(
        &self,
        epoch: u64,
        item: luna_proto::remote_projection::RemoteTasksItem,
    ) -> Result<(), Refusal> {
        let result = {
            let mut inner = self.lock();
            let Some(follow) = inner.follow.as_mut() else {
                return Err(Refusal::Stop);
            };
            let result = follow.store.apply_tasks(epoch, item, now_ms());
            if result.is_ok() {
                follow.revision += 1;
            }
            result.map(|_| ())
        };
        self.applied(result)
    }

    /// Apply one task item from socket `epoch`; `Ok(true)` once the Mac
    /// reported the task removed.
    fn apply_task(
        &self,
        epoch: u64,
        task_id: &str,
        item: luna_proto::remote_projection::RemoteTaskItem,
    ) -> Result<bool, Refusal> {
        let result = {
            let mut inner = self.lock();
            let Some(follow) = inner.follow.as_mut() else {
                return Err(Refusal::Stop);
            };
            // A follower stopped by eviction may still hold one item.
            if !follow.followed.iter().any(|id| id == task_id) {
                return Err(Refusal::Stop);
            }
            let result = follow.store.apply_task(epoch, task_id, item, now_ms());
            if result.is_ok() {
                follow.revision += 1;
            }
            result.map(|view| view.task.is_none())
        };
        match result {
            Ok(removed) => {
                self.signal();
                Ok(removed)
            }
            Err(error) => self.applied(Err(error)).map(|()| false),
        }
    }

    fn applied(&self, result: Result<(), ProjectionError>) -> Result<(), Refusal> {
        match result {
            Ok(()) => {
                self.signal();
                Ok(())
            }
            Err(ProjectionError::StaleGeneration) => Err(Refusal::Stop),
            Err(ProjectionError::WrongHost(detail)) => {
                // A verified socket that projects another host is never
                // trusted again.
                self.terminate(SessionCloseReason::Rejected, detail);
                Err(Refusal::Stop)
            }
            Err(ProjectionError::OutOfOrder(_) | ProjectionError::Invalid(_)) => {
                Err(Refusal::Resnapshot)
            }
        }
    }

    fn registry_cursor(&self) -> Option<luna_proto::remote_projection::ProjectionCursor> {
        self.lock().follow.as_ref()?.store.registry_cursor()
    }

    fn task_cursor(
        &self,
        task_id: &str,
    ) -> Option<luna_proto::remote_projection::ProjectionCursor> {
        self.lock().follow.as_ref()?.store.task_cursor(task_id)
    }

    async fn handshake(&self) -> Result<RemoteHostInfo, RemoteSessionError> {
        // 1. The descriptor names the pinned host, its protocol and catalog.
        let (status, body, _) = self
            .http(
                "GET",
                DESCRIPTOR_PATH,
                Vec::new(),
                None,
                MAX_DESCRIPTOR_BYTES,
            )
            .await?;
        let descriptor = verify_descriptor(&self.host, status, &body).map_err(descriptor_error)?;
        // 2. The saved bearer buys a single-use socket ticket.
        let body = serde_json::to_vec(&serde_json::json!({ "target": self.host.target })).map_err(
            |error| RemoteSessionError::Engine {
                detail: format!("serialize ticket request: {error}"),
            },
        )?;
        let (status, body, route) = self
            .http(
                "POST",
                SOCKET_TICKET_PATH,
                body,
                Some(format!("Bearer {}", self.token)),
                MAX_TICKET_REPLY_BYTES,
            )
            .await?;
        let ticket = socket_ticket(status, &body)?;
        // 3. The platform opens the pinned socket with that ticket.
        let (socket_id, inbound) = self.open_socket(&ticket.ticket).await?;
        // 4. The shared SDK over the platform's frames.
        let (outbound, frames) = mpsc::channel::<String>(FRAME_QUEUE);
        self.start_pump(socket_id, frames);
        let client = Arc::new(
            luna_sdk::Client::new(RpcClient::new(outbound, inbound)).with_deadline(CALL_DEADLINE),
        );
        // 5. One typed read proves who answers on this socket.
        let info = client
            .engine_info()
            .await
            .map_err(|error| self.sdk_error(error))?;
        let host = host_info(
            &descriptor.target,
            &descriptor.protocol,
            descriptor.catalog_version,
            &info,
        )?;
        {
            let mut inner = self.lock();
            let current = inner
                .socket
                .as_ref()
                .is_some_and(|socket| socket.id == socket_id);
            if !matches!(inner.phase, Phase::Connecting) || !current {
                drop(inner);
                return Err(self.closed_error());
            }
            inner.phase = Phase::Open(host.clone());
            inner.client = Some(client);
            inner.route = Some(route);
        }
        self.notify();
        Ok(host)
    }

    /// One pinned HTTPS request through the platform.
    async fn http(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
        authorization: Option<String>,
        max_body_bytes: u32,
    ) -> Result<(u16, Vec<u8>, RouteObservation), RemoteSessionError> {
        let (reply, replied) = oneshot::channel();
        let request = {
            let mut inner = self.lock();
            if matches!(inner.phase, Phase::Closed(..)) {
                drop(inner);
                return Err(self.closed_error());
            }
            let request_id = inner.take_id();
            inner.request = Some((request_id, reply));
            EnrollmentRequest {
                request_id,
                url: format!("{}{path}", self.endpoint.origin),
                host: self.endpoint.host.clone(),
                spki_sha256: self.host.spki_sha256.clone(),
                method: method.into(),
                body,
                authorization,
                max_body_bytes,
            }
        };
        self.transport.send_request(request);
        match self.until_closed(replied).await? {
            Reply::Http { body, .. } if body.len() > max_body_bytes as usize => {
                Err(RemoteSessionError::InvalidResponse {
                    detail: format!("the Mac's reply exceeded {max_body_bytes} bytes"),
                })
            }
            Reply::Http {
                status,
                body,
                route,
            } => Ok((status, body, route)),
            Reply::Failed(failure) => Err(transport_error(failure)),
        }
    }

    async fn open_socket(
        &self,
        ticket: &str,
    ) -> Result<(u64, mpsc::Receiver<String>), RemoteSessionError> {
        let (opened, open_reply) = oneshot::channel();
        let (inbound, frames) = mpsc::channel(FRAME_QUEUE);
        let request = {
            let mut inner = self.lock();
            if matches!(inner.phase, Phase::Closed(..)) {
                drop(inner);
                return Err(self.closed_error());
            }
            let socket_id = inner.take_id();
            inner.socket = Some(Socket {
                id: socket_id,
                opened: Some(opened),
                inbound,
            });
            let authority = self
                .endpoint
                .origin
                .strip_prefix("https://")
                .unwrap_or(&self.endpoint.origin);
            SocketOpenRequest {
                socket_id,
                url: format!("wss://{authority}{SOCKET_PATH}"),
                host: self.endpoint.host.clone(),
                spki_sha256: self.host.spki_sha256.clone(),
                authorization: format!("Ticket {ticket}"),
                max_message_bytes: MAX_MESSAGE_BYTES,
            }
        };
        let socket_id = request.socket_id;
        self.transport.open_socket(request);
        match self.until_closed(open_reply).await? {
            Ok(()) => Ok((socket_id, frames)),
            Err(failure) => Err(transport_error(failure)),
        }
    }

    /// Carry the RPC client's frames to the platform socket, in order.
    fn start_pump(&self, socket_id: u64, mut frames: mpsc::Receiver<String>) {
        let transport = self.transport.clone();
        let pump = runtime().spawn(async move {
            while let Some(frame) = frames.recv().await {
                transport.send_frame(socket_id, frame);
            }
        });
        self.track(pump.abort_handle());
    }

    async fn read_engine(&self) -> Result<RemoteHostInfo, RemoteSessionError> {
        let (client, current) = self.open_client()?;
        let info = client
            .engine_info()
            .await
            .map_err(|error| self.sdk_error(error))?;
        match host_info(
            &self.host.target,
            &current.direct_protocol,
            current.catalog_version,
            &info,
        ) {
            Ok(host) => Ok(host),
            Err(error) => {
                // A socket that changes who answers is never trusted again.
                self.terminate(error.close_reason(), error.to_string());
                Err(error)
            }
        }
    }

    async fn watch(
        &self,
        task_id: String,
    ) -> Result<Arc<TaskCommandWatchHandle>, RemoteSessionError> {
        if task_id.is_empty() || task_id.len() > MAX_TASK_ID_BYTES {
            return Err(RemoteSessionError::InvalidRequest {
                detail: format!("a task id is 1-{MAX_TASK_ID_BYTES} bytes"),
            });
        }
        let (client, _) = self.open_client()?;
        let (updates, receiver) = mpsc::channel(WATCH_QUEUE);
        let (ready, is_ready) = oneshot::channel::<Result<(), RemoteSessionError>>();
        let task = runtime().spawn(async move {
            // The SDK watch completes its first page and repairs gaps; this
            // task only carries its updates. Dropping it (abort) drops the
            // watch, whose subscription cancels the engine's stream.
            let mut watch = match client.watch_task_commands(&task_id, None, None).await {
                Ok(watch) => watch,
                Err(error) => {
                    let _ = ready.send(Err(from_sdk(error)));
                    return;
                }
            };
            let _ = ready.send(Ok(()));
            while let Some(update) = watch.next().await {
                let update = update.map(change).map_err(from_sdk);
                let failed = update.is_err();
                if updates.send(update).await.is_err() || failed {
                    break;
                }
            }
        });
        // Created before waiting: if this future is dropped (cancelled), the
        // handle's drop aborts the task and its engine stream.
        let handle = Arc::new(TaskCommandWatchHandle {
            updates: tokio::sync::Mutex::new(receiver),
            task: task.abort_handle(),
        });
        self.track(task.abort_handle());
        self.until_closed(is_ready).await??;
        Ok(handle)
    }
}

/// RM-03: follow the registry on socket `epoch` until that socket ends.
/// Each (re)subscription resumes from the held cursor; a stream the store
/// refuses is replaced by one without a cursor, which starts with a snapshot.
async fn run_registry_follower(core: Arc<Core>, client: Arc<SdkClient>, epoch: u64) {
    let mut fresh = false;
    let mut failures = 0u32;
    loop {
        let after = if fresh { None } else { core.registry_cursor() };
        fresh = false;
        match client.watch_remote_tasks(after).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    let applied = match item {
                        Ok(item) => core.apply_registry(epoch, item),
                        Err(_) => Err(Refusal::Resnapshot),
                    };
                    match applied {
                        Ok(()) => failures = 0,
                        Err(Refusal::Stop) => return,
                        Err(Refusal::Resnapshot) => {
                            fresh = true;
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                if let Some(issue) = follow_refused(&error) {
                    core.set_issue(epoch, issue);
                    return;
                }
            }
        }
        if !core.is_epoch(epoch) {
            return;
        }
        failures += 1;
        if failures > MAX_FOLLOW_RETRIES {
            core.set_issue(
                epoch,
                "The Mac's task list stopped updating; it retries on the next connection".into(),
            );
            return;
        }
        tokio::time::sleep(FIRST_RETRY * failures).await;
    }
}

/// RM-03: follow one task on socket `epoch` until that socket ends or the
/// Mac reports the task removed.
async fn run_task_follower(core: Arc<Core>, client: Arc<SdkClient>, epoch: u64, task_id: String) {
    let mut fresh = false;
    let mut failures = 0u32;
    loop {
        let after = if fresh {
            None
        } else {
            core.task_cursor(&task_id)
        };
        fresh = false;
        match client.watch_remote_task(&task_id, after).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    let applied = match item {
                        Ok(item) => core.apply_task(epoch, &task_id, item),
                        Err(_) => Err(Refusal::Resnapshot),
                    };
                    match applied {
                        Ok(true) => return,
                        Ok(false) => failures = 0,
                        Err(Refusal::Stop) => return,
                        Err(Refusal::Resnapshot) => {
                            fresh = true;
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                if let Some(issue) = follow_refused(&error) {
                    core.set_issue(epoch, issue);
                    return;
                }
            }
        }
        if !core.is_epoch(epoch) {
            return;
        }
        failures += 1;
        if failures > MAX_FOLLOW_RETRIES {
            return;
        }
        tokio::time::sleep(FIRST_RETRY * failures).await;
    }
}

/// A projection subscription the Mac refuses for good on this socket.
fn follow_refused(error: &SdkError) -> Option<String> {
    match from_sdk_ref(error) {
        RemoteSessionError::Unsupported { .. } => {
            Some("This Mac's LUNA does not share task lists yet; update LUNA on the Mac".into())
        }
        RemoteSessionError::Denied { .. } => Some(
            "This phone's access does not include reading tasks; pair again with task access"
                .into(),
        ),
        _ => None,
    }
}

/// Doubling from [`FIRST_RETRY`] to [`MAX_RETRY`], with ±20% jitter so many
/// phones do not retry in step.
fn backoff(attempt: u32) -> Duration {
    let base = FIRST_RETRY
        .saturating_mul(1u32 << attempt.min(8))
        .min(MAX_RETRY);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(attempt);
    let percent = 80 + (hasher.finish() % 41) as u32;
    base.saturating_mul(percent) / 100
}

fn now_ms() -> i64 {
    i64::try_from(now()).unwrap_or(i64::MAX)
}

/// Closes the session if a connect is dropped before it settles.
struct CloseUnlessSettled {
    core: Arc<Core>,
    settled: bool,
}

impl Drop for CloseUnlessSettled {
    fn drop(&mut self) {
        if !self.settled {
            self.core.terminate(
                SessionCloseReason::Cancelled,
                "The connection was cancelled".into(),
            );
        }
    }
}

/// The socket must answer as the descriptor's host, in its workspace scope,
/// with every capability the saved target requires.
fn host_info(
    target: &RemoteTarget,
    direct_protocol: &str,
    catalog_version: u16,
    info: &EngineInfo,
) -> Result<RemoteHostInfo, RemoteSessionError> {
    if info.device_id != target.device_id || info.workspace_scope != target.workspace_scope {
        return Err(RemoteSessionError::IdentityMismatch {
            detail: format!(
                "the socket answered as device {} ({}), expected device {} ({})",
                info.device_id,
                wire_name(&info.workspace_scope),
                target.device_id,
                wire_name(&target.workspace_scope)
            ),
        });
    }
    if let Some(missing) = target
        .required_capabilities
        .iter()
        .find(|capability| !info.supports(capability))
    {
        return Err(RemoteSessionError::Incompatible {
            detail: format!("the Mac's engine does not advertise {missing}"),
        });
    }
    Ok(RemoteHostInfo {
        device_id: info.device_id.clone(),
        environment_id: target.environment_id.clone(),
        workspace_scope: wire_name(&info.workspace_scope),
        profile_id: info.profile_id.clone(),
        capabilities: info.capabilities.clone(),
        direct_protocol: direct_protocol.into(),
        catalog_version,
        api_version: luna_sdk::API_VERSION,
    })
}

fn socket_ticket(status: u16, body: &[u8]) -> Result<SocketTicket, RemoteSessionError> {
    match status {
        200 => {}
        401 | 403 => {
            return Err(RemoteSessionError::RePair {
                detail: "Access was revoked or expired; pair again on the Mac".into(),
            });
        }
        503 => {
            return Err(RemoteSessionError::Offline {
                detail: "Remote access is turned off on the Mac".into(),
            });
        }
        other => {
            return Err(RemoteSessionError::InvalidResponse {
                detail: format!("socket ticket: HTTP status {other}"),
            });
        }
    }
    let ticket: SocketTicket =
        serde_json::from_slice(body).map_err(|error| RemoteSessionError::InvalidResponse {
            detail: format!("socket ticket: {error}"),
        })?;
    // It becomes a header value: bounded, printable, no whitespace. Expiry is
    // the host's to enforce; a phone clock is not trusted to judge it.
    if ticket.ticket.is_empty()
        || ticket.ticket.len() > MAX_TICKET_BYTES
        || !ticket.ticket.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(RemoteSessionError::InvalidResponse {
            detail: "socket ticket is malformed".into(),
        });
    }
    Ok(ticket)
}

fn descriptor_error((reason, detail): (RejectReason, String)) -> RemoteSessionError {
    match reason {
        RejectReason::UnsupportedVersion => RemoteSessionError::Incompatible { detail },
        RejectReason::PinMismatch | RejectReason::TargetMismatch => {
            RemoteSessionError::IdentityMismatch { detail }
        }
        RejectReason::TrustRejected => RemoteSessionError::TrustRejected { detail },
        RejectReason::HttpStatus | RejectReason::Network => RemoteSessionError::Offline { detail },
        RejectReason::Body => RemoteSessionError::InvalidResponse { detail },
    }
}

fn transport_error(failure: TransportFailure) -> RemoteSessionError {
    match failure {
        TransportFailure::TrustRejected { detail } => RemoteSessionError::TrustRejected {
            detail: format!("TLS trust was refused: {detail}"),
        },
        TransportFailure::Network { detail } => RemoteSessionError::Offline { detail },
        TransportFailure::TooLarge => RemoteSessionError::InvalidResponse {
            detail: "the Mac's reply exceeded the size limit".into(),
        },
    }
}

/// The SDK's error kinds, which the TypeScript SDK shares, as session errors.
fn from_sdk(error: SdkError) -> RemoteSessionError {
    from_sdk_ref(&error)
}

fn from_sdk_ref(error: &SdkError) -> RemoteSessionError {
    let detail = error.to_string();
    match error.kind() {
        "offline" => RemoteSessionError::Offline { detail },
        "timeout" => RemoteSessionError::Timeout { detail },
        "unsupported" => RemoteSessionError::Unsupported { detail },
        "incompatible" => RemoteSessionError::Incompatible { detail },
        "invalidRequest" => RemoteSessionError::InvalidRequest { detail },
        "invalidResponse" => RemoteSessionError::InvalidResponse { detail },
        _ if detail.starts_with(DENIED_PREFIX) => RemoteSessionError::Denied { detail },
        _ => RemoteSessionError::Engine { detail },
    }
}

fn change(update: TaskCommandUpdate) -> TaskCommandChange {
    match update {
        TaskCommandUpdate::Snapshot { read } => TaskCommandChange::Snapshot {
            state: state_of(&read.snapshot),
            events: read
                .events
                .iter()
                .map(|event| entry_of(&event.receipt, Some(event.task_sequence)))
                .collect(),
        },
        TaskCommandUpdate::Event { receipt } => TaskCommandChange::Event {
            entry: entry_of(&receipt, Some(receipt.snapshot.task_sequence)),
            state: state_of(&receipt.snapshot),
        },
        TaskCommandUpdate::Progress { receipt } => TaskCommandChange::Progress {
            entry: entry_of(&receipt, None),
        },
        TaskCommandUpdate::Reload { snapshot } => TaskCommandChange::Reload {
            state: state_of(&snapshot),
        },
    }
}

fn state_of(snapshot: &TaskCommandSnapshot) -> TaskCommandState {
    TaskCommandState {
        task_id: snapshot.task_id.clone(),
        task_revision: snapshot.task_revision,
        task_sequence: snapshot.task_sequence,
        binding_generation: snapshot.binding_generation,
        active_turn_id: snapshot.active_turn_id.clone(),
    }
}

fn entry_of(receipt: &CommandReceipt, task_sequence: Option<u64>) -> TaskCommandEntry {
    TaskCommandEntry {
        command_id: receipt.command_id.clone(),
        intent: receipt.intent.clone(),
        status: wire_name(&receipt.status),
        reason_code: receipt
            .reason
            .as_ref()
            .map(|reason| wire_name(&reason.code)),
        task_sequence,
    }
}

/// A unit enum's wire spelling (`applied`, `staleTaskRevision`, `local`).
fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
