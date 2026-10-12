//! The engine-owned direct gateway (RM-01): the M0 listener rules and pinned
//! identity, plus pairing, session checks, socket tickets and the RPC
//! WebSocket. Every socket runs the per-connection remote guard
//! ([`RemoteRpcService`]) over the engine's restricted remote service, so a
//! remote caller reaches only catalogued remote-scoped methods. Nothing is
//! proxied to local-owner IPC.
//!
//! Lifecycle: [`Gateway::stop`] first closes admission (every route and every
//! socket call is refused), then closes every socket and stops the listener.
//! Revoking a session refuses its calls at once and closes its sockets.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{
    ALLOW, AUTHORIZATION, CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, ORIGIN,
    SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE,
};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use luna_proto::remote_control::{
    DESCRIPTOR_PATH, HEALTH_PATH, PAIR_PATH, REMOTE_DIRECT_V1, RemoteTarget, SESSION_CHECK_PATH,
    SOCKET_PATH, SOCKET_TICKET_PATH,
};
use luna_rpc::remote_auth::{RedeemPairing, RemoteRpcService};
use luna_rpc::{RemoteAuthStore, RpcError, RpcReply, RpcService};
use serde::Deserialize;
use tokio::sync::{Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::host::{BindError, bind};
use crate::identity::{HostIdentity, IdentityError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_IN_FLIGHT: usize = 32;
const STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error(transparent)]
    Bind(#[from] BindError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("the TLS identity speaks for another host or profile")]
    TargetMismatch,
    #[error("listener address unavailable: {0}")]
    Io(#[from] std::io::Error),
}

/// What the engine supplies: its profile's identity and authority store, and
/// its restricted remote service.
pub struct GatewayParts {
    pub identity: HostIdentity,
    pub auth: Arc<RemoteAuthStore>,
    pub service: Arc<dyn RpcService>,
}

struct Shared {
    /// Cleared first on stop: every route and socket call is refused.
    open: AtomicBool,
    shutdown: CancellationToken,
    connections: TaskTracker,
    slots: Arc<Semaphore>,
    acceptor: TlsAcceptor,
    auth: Arc<RemoteAuthStore>,
    service: Arc<dyn RpcService>,
    descriptor: Bytes,
    health: Bytes,
}

pub struct Gateway {
    address: SocketAddr,
    spki_sha256: String,
    target: RemoteTarget,
    shared: Arc<Shared>,
}

impl Gateway {
    pub async fn start(bind_to: SocketAddr, parts: GatewayParts) -> Result<Self, GatewayError> {
        let expected = parts.auth.target();
        let target = parts.identity.target().clone();
        if target.device_id != expected.device_id
            || target.workspace_scope != expected.workspace_scope
            || target.environment_id != expected.environment_id
        {
            return Err(GatewayError::TargetMismatch);
        }
        let config = parts.identity.server_config()?;
        let descriptor = crate::descriptor(&parts.identity);
        let listener = bind(bind_to).await?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            open: AtomicBool::new(true),
            shutdown: CancellationToken::new(),
            connections: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            acceptor: TlsAcceptor::from(config),
            auth: parts.auth,
            service: parts.service,
            descriptor: Bytes::from(
                serde_json::to_vec(&descriptor).expect("descriptor serializes"),
            ),
            health: Bytes::from(
                serde_json::to_vec(&serde_json::json!({ "protocol": REMOTE_DIRECT_V1 }))
                    .expect("health serializes"),
            ),
        });
        let accepting = shared.clone();
        shared.connections.spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = accepting.shutdown.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((tcp, peer)) = accepted else {
                    continue;
                };
                let Ok(slot) = accepting.slots.clone().try_acquire_owned() else {
                    // Over the bound: close instead of queueing without limit.
                    drop(tcp);
                    continue;
                };
                let connection = accepting.clone();
                accepting.connections.spawn(async move {
                    let _slot = slot;
                    tokio::select! {
                        _ = connection.shutdown.cancelled() => {}
                        _ = serve_connection(connection.clone(), tcp, peer) => {}
                    }
                });
            }
        });
        Ok(Self {
            address,
            spki_sha256: descriptor.spki_sha256,
            target,
            shared,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn spki_sha256(&self) -> &str {
        &self.spki_sha256
    }

    pub fn target(&self) -> &RemoteTarget {
        &self.target
    }

    /// Refuse every new admission, then close every socket and the listener.
    pub async fn stop(self) {
        self.shared.open.store(false, Ordering::SeqCst);
        self.shared.shutdown.cancel();
        self.shared.connections.close();
        if tokio::time::timeout(STOP_WAIT, self.shared.connections.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                still_running = self.shared.connections.len(),
                "remote gateway connections still closing"
            );
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.shared.open.store(false, Ordering::SeqCst);
        self.shared.shutdown.cancel();
    }
}

async fn serve_connection(shared: Arc<Shared>, tcp: tokio::net::TcpStream, peer: SocketAddr) {
    let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, shared.acceptor.accept(tcp)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => {
            tracing::debug!(%peer, %error, "remote TLS handshake failed");
            return;
        }
        Err(_) => return,
    };
    let routes = shared.clone();
    let service = hyper::service::service_fn(move |request| {
        let shared = routes.clone();
        async move { Ok::<_, Infallible>(route(shared, request).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .max_buf_size(MAX_HEADER_BYTES)
        .serve_connection(TokioIo::new(tls), service)
        .with_upgrades()
        .await;
}

async fn route(shared: Arc<Shared>, request: Request<Incoming>) -> Response<Full<Bytes>> {
    if !shared.open.load(Ordering::SeqCst) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "remote access is disabled");
    }
    let path = request.uri().path().to_owned();
    let allowed = match path.as_str() {
        HEALTH_PATH | DESCRIPTOR_PATH | SOCKET_PATH => Method::GET,
        PAIR_PATH | SESSION_CHECK_PATH | SOCKET_TICKET_PATH => Method::POST,
        _ => return reply(StatusCode::NOT_FOUND, Bytes::new()),
    };
    if request.method() != allowed {
        let mut response = reply(StatusCode::METHOD_NOT_ALLOWED, Bytes::new());
        response
            .headers_mut()
            .insert(ALLOW, allowed.as_str().parse().expect("static header"));
        return response;
    }
    match path.as_str() {
        HEALTH_PATH => reply(StatusCode::OK, shared.health.clone()),
        DESCRIPTOR_PATH => reply(StatusCode::OK, shared.descriptor.clone()),
        SOCKET_PATH => upgrade(shared, request),
        _ => {
            let bearer = bearer(&request);
            let body = match read_body(request).await {
                Ok(body) => body,
                Err(response) => return response,
            };
            match path.as_str() {
                PAIR_PATH => match serde_json::from_slice::<RedeemPairing>(&body) {
                    Ok(redeem) => answer(shared.auth.redeem(redeem)),
                    Err(_) => error(StatusCode::BAD_REQUEST, "invalid pairing request"),
                },
                SESSION_CHECK_PATH | SOCKET_TICKET_PATH => {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Target {
                        target: RemoteTarget,
                    }
                    let Some(token) = bearer else {
                        return error(StatusCode::UNAUTHORIZED, "device session required");
                    };
                    let Ok(Target { target }) = serde_json::from_slice(&body) else {
                        return error(StatusCode::BAD_REQUEST, "invalid session request");
                    };
                    if path == SESSION_CHECK_PATH {
                        answer(
                            shared
                                .auth
                                .authenticate(&target, &token)
                                .and_then(|session| shared.auth.record_use(&session.id)),
                        )
                    } else {
                        answer(shared.auth.issue_socket_ticket(&target, &token))
                    }
                }
                _ => reply(StatusCode::NOT_FOUND, Bytes::new()),
            }
        }
    }
}

/// Bodies are bounded before they are read: a declared length over the
/// limit is refused outright, and a streamed body stops at the limit.
async fn read_body(request: Request<Incoming>) -> Result<Bytes, Response<Full<Bytes>>> {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        ));
    }
    Limited::new(request.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
        .map(|collected| collected.to_bytes())
        .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"))
}

fn bearer(request: &Request<Incoming>) -> Option<String> {
    request
        .headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_owned)
}

fn upgrade(shared: Arc<Shared>, mut request: Request<Incoming>) -> Response<Full<Bytes>> {
    // A browser always sends `Origin` on a WebSocket handshake and cannot
    // suppress it; the phone's native client sends none.
    if request.headers().contains_key(ORIGIN) {
        return error(StatusCode::FORBIDDEN, "origin not allowed");
    }
    let headers = request.headers();
    let is_upgrade = headers
        .get(UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && headers
            .get(SEC_WEBSOCKET_VERSION)
            .is_some_and(|value| value == "13");
    let Some(key) = headers
        .get(SEC_WEBSOCKET_KEY)
        .cloned()
        .filter(|_| is_upgrade)
    else {
        return error(StatusCode::BAD_REQUEST, "websocket upgrade required");
    };
    let Some(ticket) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Ticket "))
    else {
        return error(StatusCode::UNAUTHORIZED, "socket ticket required");
    };
    let session = match shared.auth.redeem_socket_ticket(ticket) {
        Ok(session) => session,
        Err(denied) => return error(StatusCode::UNAUTHORIZED, &denied.to_string()),
    };
    let upgraded = hyper::upgrade::on(&mut request);
    let socket = shared.clone();
    shared.connections.spawn(async move {
        match upgraded.await {
            Ok(io) => run_socket(socket, TokioIo::new(io), session).await,
            Err(error) => tracing::debug!(%error, "remote websocket upgrade failed"),
        }
    });
    let mut response = reply(StatusCode::SWITCHING_PROTOCOLS, Bytes::new());
    let headers = response.headers_mut();
    headers.insert(CONNECTION, "Upgrade".parse().expect("static header"));
    headers.insert(UPGRADE, "websocket".parse().expect("static header"));
    headers.insert(
        SEC_WEBSOCKET_ACCEPT,
        derive_accept_key(key.as_bytes())
            .parse()
            .expect("accept key is a header value"),
    );
    response
}

/// One authenticated socket: the session's guard behind the admission gate
/// and the in-flight bound. Closes on stop, on the session's revocation or
/// expiry, or when the peer leaves.
async fn run_socket<S>(shared: Arc<Shared>, io: S, session: String)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let config = WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE_BYTES),
        max_frame_size: Some(MAX_MESSAGE_BYTES),
        ..Default::default()
    };
    let socket = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
    if let Err(denied) = shared.auth.record_use(&session) {
        tracing::debug!(%denied, "remote socket refused at open");
        return;
    }
    let service: Arc<dyn RpcService> = Arc::new(Admitted {
        shared: shared.clone(),
        slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        inner: Arc::new(RemoteRpcService::for_session(
            shared.service.clone(),
            shared.auth.clone(),
            session.clone(),
        )),
    });
    let stop = shared.shutdown.child_token();
    let watcher = {
        let (auth, stop, session) = (shared.auth.clone(), stop.clone(), session.clone());
        let mut changes = auth.subscribe();
        async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    changed = changes.changed() => {
                        if changed.is_err() || auth.validate_session(&session).is_err() {
                            stop.cancel();
                            return;
                        }
                    }
                }
            }
        }
    };
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(256);
    let pump = {
        let stop = stop.clone();
        async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    frame = out_rx.recv() => match frame {
                        Some(text) => if sink.send(Message::Text(text)).await.is_err() { break },
                        None => break,
                    },
                    message = stream.next() => match message {
                        Some(Ok(Message::Text(text))) => if in_tx.send(text).await.is_err() { break },
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {}
                    },
                }
            }
            let _ = sink.send(Message::Close(None)).await;
            stop.cancel();
        }
    };
    tokio::join!(pump, watcher, async {
        tokio::select! {
            _ = stop.cancelled() => {}
            _ = luna_rpc::serve_connection(service, out_tx, in_rx) => {}
        }
    });
}

/// Refuses every call once the gateway stops admitting, and bounds the
/// requests and streams one socket can hold open.
struct Admitted {
    shared: Arc<Shared>,
    slots: Arc<Semaphore>,
    inner: Arc<dyn RpcService>,
}

#[async_trait]
impl RpcService for Admitted {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if !self.shared.open.load(Ordering::SeqCst) {
            return Err(RpcError::Failed(
                "remote authorization denied: remote access is disabled".into(),
            ));
        }
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return Err(RpcError::Failed(
                "too many remote requests in flight".into(),
            ));
        };
        match self.inner.handle(method, params).await? {
            RpcReply::Stream(stream) => Ok(RpcReply::Stream(Box::pin(stream.map(move |item| {
                let _held = &slot;
                item
            })))),
            value => Ok(value),
        }
    }
}

fn answer<T: serde::Serialize>(result: Result<T, RpcError>) -> Response<Full<Bytes>> {
    match result {
        Ok(value) => reply(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&value).expect("reply serializes")),
        ),
        Err(denied) => error(StatusCode::FORBIDDEN, &denied.to_string()),
    }
}

fn error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "error": message })).expect("error serializes");
    reply(status, Bytes::from(body))
}

fn reply(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    let has_body = !body.is_empty();
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, "no-store".parse().expect("static header"));
    if has_body {
        headers.insert(
            CONTENT_TYPE,
            "application/json".parse().expect("static header"),
        );
    }
    response
}
