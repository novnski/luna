//! The SDK session facade (IOS-00): one paired Mac's typed operations through
//! the shared `luna-sdk` client, over a socket the platform owns.
//!
//! The split is the one RM-00 proved. Swift performs every network operation
//! with `URLSession` and evaluates the TLS pin; this core decides what each
//! result means and runs the public SDK over the frames Swift carries. There
//! is no second client: frames go through `luna_rpc::RpcClient` and every
//! operation through `luna_sdk::Client`, exactly as the desktop app and the
//! CLI call the engine.
//!
//! `connect` opens one socket:
//! 1. the pinned descriptor must name the saved host, protocol and the
//!    catalog this SDK speaks (before any credential is used);
//! 2. the saved bearer buys one single-use socket ticket;
//! 3. the platform opens the pinned socket with that ticket;
//! 4. one typed SDK read (`engineInfo`) must answer as the same host.
//!
//! A session never reconnects: once closed it stays closed, and the caller
//! creates a new one. RM-03 owns reconnect, caches and projections on top of
//! this type; it extends it rather than adding another connection owner.
//!
//! Threading: every method is callable from any thread. Platform callbacks
//! may arrive on any thread, synchronously or late; replies for anything but
//! the current request or socket are ignored. No lock is held while foreign
//! code runs. The listener is told *that* the state changed, from whichever
//! thread changed it; the platform reads [`RemoteSession::state`] on its own
//! actor, so out-of-order notifications cannot reorder states.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use luna_proto::remote_control::{
    DESCRIPTOR_PATH, HostTicket, RemoteTarget, SOCKET_PATH, SOCKET_TICKET_PATH,
};
use luna_proto::{CommandReceipt, EngineInfo, TaskCommandSnapshot};
use luna_rpc::RpcClient;
use luna_rpc::remote_auth::SocketTicket;
use luna_sdk::{SdkError, TaskCommandUpdate};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

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

/// The platform half of a session: pinned HTTPS requests and one pinned
/// WebSocket. Each request is reported once, through
/// [`RemoteSession::request_completed`] or [`RemoteSession::request_failed`];
/// the socket through [`RemoteSession::socket_opened`], then
/// [`RemoteSession::receive`] for each text message in order, then
/// [`RemoteSession::socket_closed`]. Calls may come from any thread.
#[uniffi::export(with_foreign)]
pub trait SocketTransport: Send + Sync {
    fn send_request(&self, request: EnrollmentRequest);
    fn open_socket(&self, request: SocketOpenRequest);
    /// Send one text message on the open socket, after every earlier one.
    fn send_frame(&self, socket_id: u64, frame: String);
    /// Stop everything for this session: cancel any request and close the
    /// socket. Idempotent; nothing needs to be reported afterwards.
    fn close(&self);
}

/// Told that [`RemoteSession::state`] changed. Called on whichever thread
/// changed it: hop to the platform's actor, then read the state there.
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
    /// The next change. `None` once the watch was cancelled, its session
    /// closed or the engine ended the stream: open a new watch, which starts
    /// from a fresh snapshot (RM-03 owns cursors and repair across sockets).
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
    /// Validate saved access with the same rules as restore; nothing touches
    /// the network until [`RemoteSession::connect`].
    #[uniffi::constructor]
    pub fn new(
        mac: SavedMac,
        endpoint_index: u32,
        transport: Arc<dyn SocketTransport>,
        listener: Option<Arc<dyn RemoteSessionListener>>,
    ) -> Result<Arc<Self>, RemoteSessionError> {
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
        Ok(Arc::new(Self {
            core: Arc::new(Core {
                host,
                endpoint,
                token: mac.session_token,
                transport,
                listener,
                closed: watch::Sender::new(false),
                inner: Mutex::new(Inner {
                    phase: Phase::Idle,
                    next_id: 1,
                    request: None,
                    socket: None,
                    client: None,
                    route: None,
                    tasks: Vec::new(),
                }),
            }),
        }))
    }

    /// Open and verify the socket. Once; a failed, cancelled or closed
    /// session stays closed.
    pub async fn connect(
        &self,
        cancel: Option<Arc<Cancellation>>,
    ) -> Result<RemoteHostInfo, RemoteSessionError> {
        let core = self.core.clone();
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

    pub fn state(&self) -> RemoteSessionState {
        self.core.state()
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
            self.core.terminate(
                SessionCloseReason::Failed,
                format!("The Mac sent a message over {MAX_MESSAGE_BYTES} bytes"),
            );
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
    /// The outbound pump and every watch; all abort when the session closes.
    tasks: Vec<AbortHandle>,
}

impl Inner {
    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

struct Core {
    host: HostTicket,
    endpoint: Endpoint,
    token: String,
    transport: Arc<dyn SocketTransport>,
    listener: Option<Arc<dyn RemoteSessionListener>>,
    closed: watch::Sender<bool>,
    inner: Mutex<Inner>,
}

impl Core {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn notify(&self) {
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
            let mut inner = self.lock();
            if matches!(inner.phase, Phase::Closed(..)) {
                return;
            }
            inner.phase = Phase::Closed(reason, detail);
            (
                inner.socket.take(),
                inner.client.take(),
                inner.request.take(),
                std::mem::take(&mut inner.tasks),
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

    async fn connect(self: Arc<Self>) -> Result<RemoteHostInfo, RemoteSessionError> {
        let refused = {
            let mut inner = self.lock();
            let refused = match &inner.phase {
                Phase::Idle => None,
                Phase::Connecting | Phase::Open(_) => Some(RemoteSessionError::Busy),
                Phase::Closed(_, detail) => Some(RemoteSessionError::Closed {
                    detail: detail.clone(),
                }),
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
            if !matches!(inner.phase, Phase::Connecting) {
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
