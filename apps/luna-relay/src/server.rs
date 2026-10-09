use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::protocol::{
    BRIDGE_SECS, Control, HANDSHAKE_SECS, HEARTBEAT_SECS, MAX_ACTIVE_PER_ROUTE, MAX_MESSAGE,
    RelayConfig, VERSION, proof, proof_matches, random_hex, validate_id,
};

type Socket = WebSocketStream<TcpStream>;

const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DATA_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Host {
    generation: u64,
    tx: mpsc::Sender<Control>,
    stop: watch::Sender<bool>,
}

struct Pending {
    route: String,
    generation: u64,
    deliver: oneshot::Sender<Socket>,
}

struct State {
    config: Option<RelayConfig>,
    revision: u64,
    next_generation: u64,
    hosts: HashMap<String, Host>,
    pending: HashMap<String, Pending>,
    active: HashMap<String, usize>,
}

struct Shared {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reload(&self, config: Option<RelayConfig>) {
        let mut state = self.lock();
        if state.config == config {
            return;
        }
        state.config = config;
        state.revision = state.revision.wrapping_add(1);
        for (_, host) in state.hosts.drain() {
            host.stop.send_replace(true);
        }
        state.pending.clear();
        self.changed.send_replace(state.revision);
    }
}

struct Reservation {
    shared: Arc<Shared>,
    route: String,
    stream_id: String,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.pending.remove(&self.stream_id);
        if let Some(count) = state.active.get_mut(&self.route) {
            if *count <= 1 {
                state.active.remove(&self.route);
            } else {
                *count -= 1;
            }
        }
    }
}

struct HostLease {
    shared: Arc<Shared>,
    route: String,
    generation: u64,
}

impl Drop for HostLease {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if state
            .hosts
            .get(&self.route)
            .is_some_and(|host| host.generation == self.generation)
        {
            if let Some(host) = state.hosts.remove(&self.route) {
                host.stop.send_replace(true);
            }
            state.pending.retain(|_, pending| {
                pending.route != self.route || pending.generation != self.generation
            });
        }
    }
}

fn fence_closed(
    stop: &watch::Receiver<bool>,
    changed: &watch::Receiver<u64>,
    revision: u64,
) -> bool {
    *stop.borrow() || *changed.borrow() != revision
}

async fn send_bounded<S>(sink: &mut S, message: Message, timeout: Duration) -> Result<()>
where
    S: futures::Sink<Message, Error = WsError> + Unpin,
{
    tokio::time::timeout(timeout, sink.send(message))
        .await
        .context("relay output timed out")??;
    Ok(())
}

async fn send_guarded<S>(
    sink: &mut S,
    message: Message,
    stop: &mut watch::Receiver<bool>,
    changed: &mut watch::Receiver<u64>,
    revision: u64,
    timeout: Duration,
) -> Result<()>
where
    S: futures::Sink<Message, Error = WsError> + Unpin,
{
    if fence_closed(stop, changed, revision) {
        bail!("relay route closed");
    }
    tokio::select! {
        biased;
        _ = stop.changed() => bail!("relay host stopped"),
        _ = changed.changed() => bail!("relay configuration changed"),
        result = send_bounded(sink, message, timeout) => result,
    }
}

pub fn socket_config() -> WebSocketConfig {
    WebSocketConfig {
        write_buffer_size: 64 * 1024,
        max_write_buffer_size: 2 * MAX_MESSAGE,
        max_message_size: Some(MAX_MESSAGE),
        max_frame_size: Some(MAX_MESSAGE),
        ..WebSocketConfig::default()
    }
}

/// Bind only to loopback. A separate owner-managed TLS reverse proxy may
/// publish this private listener after an exact deployment is authorized.
pub async fn serve(
    listener: TcpListener,
    config_path: PathBuf,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        bail!("relay listener must bind loopback behind owner-managed TLS");
    }
    let config = RelayConfig::load(&config_path)?;
    let (changed, _) = watch::channel(0);
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            config: Some(config),
            revision: 0,
            next_generation: 0,
            hosts: HashMap::new(),
            pending: HashMap::new(),
            active: HashMap::new(),
        }),
        changed,
    });
    let slots = Arc::new(tokio::sync::Semaphore::new(128));
    let mut tasks = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    let mut reload = tokio::time::interval(Duration::from_secs(1));
    reload.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            _ = reload.tick() => {
                let updated = RelayConfig::load(&config_path);
                if let Err(error) = &updated {
                    tracing::warn!(%error, "relay configuration unavailable; closing routes");
                }
                shared.reload(updated.ok());
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept relay socket")?;
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                let shared = shared.clone();
                tasks.spawn(async move {
                    let _slot = slot;
                    if let Err(error) = handle_socket(stream, shared).await {
                        tracing::debug!(%error, "relay connection closed");
                    }
                });
            }
        }
    }
    shared.reload(None);
    tasks.shutdown().await;
    Ok(())
}

#[derive(Debug)]
enum Role {
    Host(String),
    Client(String, String),
    Bridge(String, String),
}

impl Role {
    fn parse(path: &str) -> Result<Self> {
        let parts: Vec<_> = path.split('/').collect();
        let role = match parts.as_slice() {
            ["", "v1", "host", route] => Self::Host((*route).to_owned()),
            ["", "v1", "client", route, client] => {
                validate_id(client)?;
                Self::Client((*route).to_owned(), (*client).to_owned())
            }
            ["", "v1", "bridge", route, stream] => {
                validate_id(stream)?;
                Self::Bridge((*route).to_owned(), (*stream).to_owned())
            }
            _ => bail!("unknown relay route"),
        };
        validate_id(role.route())?;
        Ok(role)
    }

    fn route(&self) -> &str {
        match self {
            Self::Host(route) | Self::Client(route, _) | Self::Bridge(route, _) => route,
        }
    }
}

async fn handle_socket(stream: TcpStream, shared: Arc<Shared>) -> Result<()> {
    let selected = Arc::new(Mutex::new(None::<String>));
    let selected_cb = selected.clone();
    #[allow(clippy::result_large_err)]
    let callback =
        move |req: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
            if req.headers().contains_key("origin") || req.uri().query().is_some() {
                return Err(rejected_http());
            }
            if Role::parse(req.uri().path()).is_err() {
                return Err(rejected_http());
            }
            *selected_cb.lock().unwrap() = Some(req.uri().path().to_owned());
            Ok(response)
        };
    let mut ws = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_SECS),
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(socket_config())),
    )
    .await??;
    let path = selected
        .lock()
        .unwrap()
        .take()
        .context("missing relay path")?;
    let role = Role::parse(&path)?;
    let nonce = random_hex()?;
    send_control(
        &mut ws,
        &Control::Challenge {
            nonce: nonce.clone(),
        },
    )
    .await?;
    let hello =
        tokio::time::timeout(Duration::from_secs(HANDSHAKE_SECS), recv_control(&mut ws)).await??;
    let Control::Hello {
        version,
        target,
        client_id,
        generation,
        proof: supplied,
    } = hello
    else {
        bail!("expected authenticated relay hello");
    };
    let admitted_revision = {
        let state = shared.lock();
        let route = state
            .config
            .as_ref()
            .and_then(|config| config.route(role.route()));
        let key = match (&role, route) {
            (Role::Host(_), Some(route)) | (Role::Bridge(_, _), Some(route)) => {
                Some(&route.host_secret)
            }
            (Role::Client(_, id), Some(route)) => route
                .clients
                .iter()
                .find(|item| item.id == *id)
                .map(|item| &item.secret),
            _ => None,
        };
        let right_client = match &role {
            Role::Client(_, id) => client_id.as_deref() == Some(id),
            _ => client_id.is_none(),
        };
        let right_generation = matches!(role, Role::Bridge(_, _)) || generation == 0;
        let accepted = key.zip(route).is_some_and(|(key, route)| {
            version == VERSION
                && target == route.target
                && right_client
                && right_generation
                && proof(key, &nonce, &path, &target, generation)
                    .is_ok_and(|expected| proof_matches(&expected, &supplied))
        });
        accepted.then_some(state.revision)
    };
    let Some(admitted_revision) = admitted_revision else {
        let _ = send_control(
            &mut ws,
            &Control::Rejected {
                reason: "credential, version or target rejected".into(),
            },
        )
        .await;
        bail!("relay admission rejected");
    };
    match role {
        Role::Host(route) => {
            host_session(
                ws,
                shared,
                route,
                admitted_revision,
                Duration::from_secs(HEARTBEAT_SECS),
            )
            .await
        }
        Role::Client(route, id) => client_session(ws, shared, route, id, admitted_revision).await,
        Role::Bridge(route, stream) => {
            bridge_session(ws, shared, route, stream, generation, admitted_revision).await
        }
    }
}

fn rejected_http() -> ErrorResponse {
    let mut response = ErrorResponse::new(Some("route not allowed".into()));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
}

async fn host_session<S>(
    mut ws: WebSocketStream<S>,
    shared: Arc<Shared>,
    route: String,
    admitted_revision: u64,
    heartbeat_period: Duration,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (tx, mut rx) = mpsc::channel(32);
    let (stop, mut stopping) = watch::channel(false);
    let mut changed = shared.changed.subscribe();
    let generation = {
        let mut state = shared.lock();
        if state.revision != admitted_revision
            || !state
                .config
                .as_ref()
                .is_some_and(|config| config.route(&route).is_some())
        {
            bail!("host admission became stale");
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        if let Some(previous) = state.hosts.insert(
            route.clone(),
            Host {
                generation,
                tx,
                stop,
            },
        ) {
            previous.stop.send_replace(true);
        }
        generation
    };
    let _lease = HostLease {
        shared: shared.clone(),
        route: route.clone(),
        generation,
    };
    send_guarded(
        &mut ws,
        Message::Text(serde_json::to_string(&Control::Ready { generation })?),
        &mut stopping,
        &mut changed,
        admitted_revision,
        CONTROL_WRITE_TIMEOUT,
    )
    .await?;
    let (mut sink, mut input) = ws.split();
    let mut heartbeat = tokio::time::interval(heartbeat_period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = stopping.changed() => break Ok(()),
            _ = changed.changed() => break Ok(()),
            message = rx.recv() => match message {
                Some(control) => {
                    let text = serde_json::to_string(&control)?;
                    send_guarded(&mut sink, Message::Text(text), &mut stopping, &mut changed,
                        admitted_revision, CONTROL_WRITE_TIMEOUT).await?;
                }
                None => break Ok(()),
            },
            message = input.next() => match message {
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break Ok(()),
                Some(Ok(Message::Ping(payload))) => { send_guarded(&mut sink, Message::Pong(payload),
                    &mut stopping, &mut changed, admitted_revision, CONTROL_WRITE_TIMEOUT).await?; },
                Some(Ok(_)) => break Err(anyhow::anyhow!("unexpected host control frame")),
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > heartbeat_period * 3 { break Ok(()); }
                send_guarded(&mut sink, Message::Ping(Vec::new()), &mut stopping, &mut changed,
                    admitted_revision, CONTROL_WRITE_TIMEOUT).await?;
            }
        }
    }
}

async fn client_session(
    mut ws: Socket,
    shared: Arc<Shared>,
    route: String,
    id: String,
    admitted_revision: u64,
) -> Result<()> {
    let stream_id = random_hex()?;
    let (tx, rx) = oneshot::channel();
    let (host, _reservation, mut host_stop, mut changed) = reserve_client(
        &shared,
        &route,
        &id,
        admitted_revision,
        stream_id.clone(),
        tx,
    )?;
    if fence_closed(&host_stop, &changed, admitted_revision) {
        bail!("client admission became stale");
    }
    host.tx
        .try_send(Control::Open {
            stream_id,
            generation: host.generation,
        })
        .map_err(|_| anyhow::anyhow!("host control queue full or closed"))?;
    let bridge = tokio::select! {
        biased;
        _ = host_stop.changed() => bail!("host generation changed"),
        _ = changed.changed() => bail!("relay configuration changed"),
        result = tokio::time::timeout(Duration::from_secs(BRIDGE_SECS), rx) => result.context("bridge timed out")??,
    };
    send_guarded(
        &mut ws,
        Message::Text(serde_json::to_string(&Control::Ready {
            generation: host.generation,
        })?),
        &mut host_stop,
        &mut changed,
        admitted_revision,
        CONTROL_WRITE_TIMEOUT,
    )
    .await?;
    proxy_ws(ws, bridge, &mut host_stop, &mut changed, admitted_revision).await
}

fn reserve_client(
    shared: &Arc<Shared>,
    route: &str,
    id: &str,
    admitted_revision: u64,
    stream_id: String,
    deliver: oneshot::Sender<Socket>,
) -> Result<(
    Host,
    Reservation,
    watch::Receiver<bool>,
    watch::Receiver<u64>,
)> {
    let changed = shared.changed.subscribe();
    let mut state = shared.lock();
    if state.revision != admitted_revision
        || !state.config.as_ref().is_some_and(|config| {
            config
                .route(route)
                .is_some_and(|route| route.clients.iter().any(|client| client.id == id))
        })
    {
        bail!("client admission became stale");
    }
    let host = state
        .hosts
        .get(route)
        .cloned()
        .context("environment offline")?;
    let host_stop = host.stop.subscribe();
    if *host_stop.borrow() {
        bail!("host generation stopped");
    }
    let count = state.active.entry(route.to_owned()).or_default();
    if *count >= MAX_ACTIVE_PER_ROUTE {
        bail!("route connection limit reached");
    }
    *count += 1;
    state.pending.insert(
        stream_id.clone(),
        Pending {
            route: route.to_owned(),
            generation: host.generation,
            deliver,
        },
    );
    Ok((
        host,
        Reservation {
            shared: shared.clone(),
            route: route.to_owned(),
            stream_id,
        },
        host_stop,
        changed,
    ))
}

async fn bridge_session(
    mut ws: Socket,
    shared: Arc<Shared>,
    route: String,
    stream: String,
    generation: u64,
    admitted_revision: u64,
) -> Result<()> {
    let mut changed = shared.changed.subscribe();
    let (pending, mut host_stop) = {
        let mut state = shared.lock();
        if state.revision != admitted_revision
            || !state
                .hosts
                .get(&route)
                .is_some_and(|host| host.generation == generation)
        {
            bail!("stale bridge generation");
        }
        if !state
            .pending
            .get(&stream)
            .is_some_and(|entry| entry.route == route && entry.generation == generation)
        {
            bail!("unknown bridge stream");
        }
        let host_stop = state
            .hosts
            .get(&route)
            .context("host lost")?
            .stop
            .subscribe();
        if *host_stop.borrow() {
            bail!("host generation stopped");
        }
        let pending = state
            .pending
            .remove(&stream)
            .context("bridge disappeared")?;
        (pending, host_stop)
    };
    send_guarded(
        &mut ws,
        Message::Text(serde_json::to_string(&Control::Ready { generation })?),
        &mut host_stop,
        &mut changed,
        admitted_revision,
        CONTROL_WRITE_TIMEOUT,
    )
    .await?;
    if fence_closed(&host_stop, &changed, admitted_revision) {
        bail!("bridge generation stopped");
    }
    pending
        .deliver
        .send(ws)
        .map_err(|_| anyhow::anyhow!("waiting client closed"))?;
    Ok(())
}

pub async fn proxy_ws<A, B>(
    a: WebSocketStream<A>,
    b: WebSocketStream<B>,
    host_stop: &mut watch::Receiver<bool>,
    changed: &mut watch::Receiver<u64>,
    expected_revision: u64,
) -> Result<()>
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if fence_closed(host_stop, changed, expected_revision) {
        return Ok(());
    }
    let (mut a_out, mut a_in) = a.split();
    let (mut b_out, mut b_in) = b.split();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut a_pong = Instant::now();
    let mut b_pong = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = host_stop.changed() => break,
            _ = changed.changed() => break,
            message = a_in.next() => match message {
                Some(Ok(Message::Text(text))) if text.len() <= MAX_MESSAGE => {
                    send_guarded(&mut b_out, Message::Text(text), host_stop, changed,
                        expected_revision, DATA_WRITE_TIMEOUT).await?;
                }
                Some(Ok(Message::Pong(_))) => a_pong = Instant::now(),
                Some(Ok(Message::Ping(payload))) => { send_guarded(&mut a_out, Message::Pong(payload),
                    host_stop, changed, expected_revision, CONTROL_WRITE_TIMEOUT).await?; },
                _ => break,
            },
            message = b_in.next() => match message {
                Some(Ok(Message::Text(text))) if text.len() <= MAX_MESSAGE => {
                    send_guarded(&mut a_out, Message::Text(text), host_stop, changed,
                        expected_revision, DATA_WRITE_TIMEOUT).await?;
                }
                Some(Ok(Message::Pong(_))) => b_pong = Instant::now(),
                Some(Ok(Message::Ping(payload))) => { send_guarded(&mut b_out, Message::Pong(payload),
                    host_stop, changed, expected_revision, CONTROL_WRITE_TIMEOUT).await?; },
                _ => break,
            },
            _ = heartbeat.tick() => {
                if a_pong.elapsed() > Duration::from_secs(3 * HEARTBEAT_SECS)
                    || b_pong.elapsed() > Duration::from_secs(3 * HEARTBEAT_SECS) { break; }
                send_guarded(&mut a_out, Message::Ping(Vec::new()), host_stop, changed,
                    expected_revision, CONTROL_WRITE_TIMEOUT).await?;
                send_guarded(&mut b_out, Message::Ping(Vec::new()), host_stop, changed,
                    expected_revision, CONTROL_WRITE_TIMEOUT).await?;
            }
        }
    }
    if !fence_closed(host_stop, changed, expected_revision) {
        let _ = send_guarded(
            &mut a_out,
            Message::Close(None),
            host_stop,
            changed,
            expected_revision,
            Duration::from_secs(1),
        )
        .await;
        let _ = send_guarded(
            &mut b_out,
            Message::Close(None),
            host_stop,
            changed,
            expected_revision,
            Duration::from_secs(1),
        )
        .await;
    }
    Ok(())
}

pub async fn send_control<S>(ws: &mut WebSocketStream<S>, message: &Control) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_bounded(
        ws,
        Message::Text(serde_json::to_string(message)?),
        CONTROL_WRITE_TIMEOUT,
    )
    .await
}

pub async fn recv_control<S>(ws: &mut WebSocketStream<S>) -> Result<Control>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match ws.next().await {
        Some(Ok(Message::Text(text))) => Ok(serde_json::from_str(&text)?),
        _ => bail!("expected relay control text"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClientKey, RouteConfig};
    use luna_proto::WorkspaceScope;
    use luna_rpc::remote_auth::RemoteTarget;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context as TaskContext, Poll};
    use tokio::io::AsyncWriteExt;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio_tungstenite::tungstenite::protocol::Role as WsRole;

    struct FailWrite<S> {
        inner: S,
        fail: Arc<AtomicBool>,
    }

    impl<S: AsyncRead + Unpin> AsyncRead for FailWrite<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for FailWrite<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.fail.load(Ordering::SeqCst) {
                return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
            }
            Pin::new(&mut self.inner).poll_write(cx, bytes)
        }
        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.fail.load(Ordering::SeqCst) {
                return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
            }
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    fn test_shared() -> Arc<Shared> {
        let (changed, _) = watch::channel(0);
        Arc::new(Shared {
            state: Mutex::new(State {
                config: Some(RelayConfig {
                    version: VERSION,
                    routes: vec![RouteConfig {
                        id: "fixture".into(),
                        target: RemoteTarget {
                            device_id: "fixture-device".into(),
                            workspace_scope: WorkspaceScope::Development,
                            environment_id: "fixture".into(),
                            protocol_version: 1,
                            required_capabilities: vec![],
                        },
                        host_secret: "a".repeat(64),
                        clients: vec![ClientKey {
                            id: "viewer".into(),
                            secret: "b".repeat(64),
                        }],
                    }],
                }),
                revision: 0,
                next_generation: 0,
                hosts: HashMap::new(),
                pending: HashMap::new(),
                active: HashMap::new(),
            }),
            changed,
        })
    }

    #[tokio::test]
    async fn host_cannot_register_after_configuration_revoked_between_checks() {
        let shared = test_shared();
        shared.reload(None); // a handshake admitted against the earlier revision
        let (transport, _peer) = tokio::io::duplex(4096);
        let ws = WebSocketStream::from_raw_socket(transport, WsRole::Server, Some(socket_config()))
            .await;
        let result = tokio::time::timeout(
            Duration::from_millis(150),
            host_session(
                ws,
                shared.clone(),
                "fixture".into(),
                0,
                Duration::from_secs(15),
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "revoked host was left registered after reload"
        );
        assert!(shared.lock().hosts.is_empty());
    }

    fn install_test_host(shared: &Arc<Shared>, generation: u64) -> Host {
        let (tx, _) = mpsc::channel(8);
        let (stop, _) = watch::channel(false);
        let host = Host {
            generation,
            tx,
            stop,
        };
        shared.lock().hosts.insert("fixture".into(), host.clone());
        host
    }

    #[tokio::test]
    async fn client_reservation_observes_reload_and_releases_route_count() {
        let shared = test_shared();
        install_test_host(&shared, 1);
        let (deliver, _) = oneshot::channel();
        let (_host, reservation, stop, changed) =
            reserve_client(&shared, "fixture", "viewer", 0, "stream-a".into(), deliver).unwrap();
        shared.reload(None);
        assert!(fence_closed(&stop, &changed, 0));
        drop(reservation);
        let state = shared.lock();
        assert!(state.active.is_empty(), "empty active route was retained");
        assert!(state.pending.is_empty());
    }

    #[tokio::test]
    async fn client_cannot_reserve_after_admission_revision_is_revoked() {
        let shared = test_shared();
        install_test_host(&shared, 1);
        shared.reload(None);
        let (deliver, _) = oneshot::channel();
        let error = reserve_client(&shared, "fixture", "viewer", 0, "stream-a".into(), deliver)
            .err()
            .expect("stale admission must fail");
        assert!(error.to_string().contains("admission became stale"));
        assert!(shared.lock().pending.is_empty());
    }

    #[tokio::test]
    async fn client_reservation_observes_replaced_host_even_when_subscribed_late() {
        let shared = test_shared();
        install_test_host(&shared, 1);
        let (deliver, _) = oneshot::channel();
        let (_host, reservation, stop, changed) =
            reserve_client(&shared, "fixture", "viewer", 0, "stream-a".into(), deliver).unwrap();
        let (tx, _) = mpsc::channel(8);
        let (stop_tx, _) = watch::channel(false);
        let replacement = Host {
            generation: 2,
            tx,
            stop: stop_tx,
        };
        let prior = shared.lock().hosts.insert("fixture".into(), replacement);
        prior.unwrap().stop.send_replace(true);
        assert!(fence_closed(&stop, &changed, 0));
        drop(reservation);
        assert_eq!(shared.lock().hosts.get("fixture").unwrap().generation, 2);
        assert!(shared.lock().active.is_empty());
    }

    #[tokio::test]
    async fn stale_bridge_cannot_deliver_after_reload() {
        let shared = test_shared();
        install_test_host(&shared, 1);
        let (deliver, receiver) = oneshot::channel();
        let (_host, reservation, _, _) =
            reserve_client(&shared, "fixture", "viewer", 0, "stream-a".into(), deliver).unwrap();
        shared.reload(None);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (transport, _) = listener.accept().await.unwrap();
        let ws = WebSocketStream::from_raw_socket(transport, WsRole::Server, Some(socket_config()))
            .await;
        assert!(
            bridge_session(
                ws,
                shared.clone(),
                "fixture".into(),
                "stream-a".into(),
                1,
                0
            )
            .await
            .is_err()
        );
        assert!(receiver.await.is_err());
        drop(reservation);
    }

    #[tokio::test]
    async fn old_host_cleanup_does_not_remove_new_generation() {
        let shared = test_shared();
        install_test_host(&shared, 1);
        let old = HostLease {
            shared: shared.clone(),
            route: "fixture".into(),
            generation: 1,
        };
        install_test_host(&shared, 2);
        drop(old);
        assert_eq!(shared.lock().hosts.get("fixture").unwrap().generation, 2);
    }

    #[tokio::test]
    async fn already_signaled_stop_closes_proxy_without_waiting_for_another_change() {
        let (a_transport, _a_peer) = tokio::io::duplex(4096);
        let (b_transport, _b_peer) = tokio::io::duplex(4096);
        let a =
            WebSocketStream::from_raw_socket(a_transport, WsRole::Server, Some(socket_config()))
                .await;
        let b =
            WebSocketStream::from_raw_socket(b_transport, WsRole::Server, Some(socket_config()))
                .await;
        let (stop_tx, _) = watch::channel(false);
        stop_tx.send_replace(true);
        let mut stop = stop_tx.subscribe(); // subscription after the stop signal
        let (_changed_tx, mut changed) = watch::channel(0);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(150),
                proxy_ws(a, b, &mut stop, &mut changed, 0)
            )
            .await
            .is_ok(),
            "pre-signaled stop was not observed"
        );
    }

    #[tokio::test]
    async fn already_changed_revision_closes_proxy_before_first_frame() {
        let (a_transport, _a_peer) = tokio::io::duplex(4096);
        let (b_transport, _b_peer) = tokio::io::duplex(4096);
        let a =
            WebSocketStream::from_raw_socket(a_transport, WsRole::Server, Some(socket_config()))
                .await;
        let b =
            WebSocketStream::from_raw_socket(b_transport, WsRole::Server, Some(socket_config()))
                .await;
        let (_stop_tx, mut stop) = watch::channel(false);
        let (changed_tx, _) = watch::channel(0);
        changed_tx.send_replace(1);
        let mut changed = changed_tx.subscribe();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(150),
                proxy_ws(a, b, &mut stop, &mut changed, 0)
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn failed_pong_write_removes_host_and_pending_routes() {
        let shared = test_shared();
        let (host_io, peer_io) = tokio::io::duplex(4096);
        let fail = Arc::new(AtomicBool::new(false));
        let host = WebSocketStream::from_raw_socket(
            FailWrite {
                inner: host_io,
                fail: fail.clone(),
            },
            WsRole::Server,
            Some(socket_config()),
        )
        .await;
        let mut peer =
            WebSocketStream::from_raw_socket(peer_io, WsRole::Client, Some(socket_config())).await;
        let shared_task = shared.clone();
        let task = tokio::spawn(async move {
            host_session(
                host,
                shared_task,
                "fixture".into(),
                0,
                Duration::from_secs(15),
            )
            .await
        });
        let Some(Ok(Message::Text(_ready))) = peer.next().await else {
            panic!("host did not announce ready")
        };
        let (deliver, receiver) = oneshot::channel();
        let (_host, reservation, _, _) =
            reserve_client(&shared, "fixture", "viewer", 0, "pending-1".into(), deliver).unwrap();
        fail.store(true, Ordering::SeqCst);
        peer.send(Message::Ping(Vec::new())).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(
            shared.lock().hosts.is_empty(),
            "failed write left host registered"
        );
        assert!(shared.lock().pending.is_empty());
        assert!(receiver.await.is_err());
        drop(reservation);
        assert!(shared.lock().active.is_empty());
    }

    #[tokio::test]
    async fn failed_ready_write_removes_new_host() {
        let shared = test_shared();
        let (host_io, _peer_io) = tokio::io::duplex(4096);
        let ws = WebSocketStream::from_raw_socket(
            FailWrite {
                inner: host_io,
                fail: Arc::new(AtomicBool::new(true)),
            },
            WsRole::Server,
            Some(socket_config()),
        )
        .await;
        assert!(
            host_session(
                ws,
                shared.clone(),
                "fixture".into(),
                0,
                Duration::from_secs(15)
            )
            .await
            .is_err()
        );
        assert!(shared.lock().hosts.is_empty());
    }

    #[tokio::test]
    async fn stalled_control_write_has_a_deadline() {
        let (mut transport, _silent_peer) = tokio::io::duplex(32);
        transport.write_all(&[0_u8; 32]).await.unwrap();
        let mut ws =
            WebSocketStream::from_raw_socket(transport, WsRole::Server, Some(socket_config()))
                .await;
        // The production deadline itself, with no test-only shortening.
        let outcome = tokio::time::timeout(
            CONTROL_WRITE_TIMEOUT + Duration::from_secs(1),
            send_control(
                &mut ws,
                &Control::Rejected {
                    reason: "x".repeat(64 * 1024),
                },
            ),
        )
        .await;
        assert!(
            outcome.is_ok_and(|sent| sent.is_err()),
            "control output waited indefinitely behind stalled transport"
        );
    }

    #[tokio::test]
    async fn stop_interrupts_stalled_heartbeat_ping() {
        let (mut a_transport, _a_peer) = tokio::io::duplex(32);
        a_transport.write_all(&[0_u8; 32]).await.unwrap();
        let (b_transport, _b_peer) = tokio::io::duplex(32);
        let a =
            WebSocketStream::from_raw_socket(a_transport, WsRole::Server, Some(socket_config()))
                .await;
        let b =
            WebSocketStream::from_raw_socket(b_transport, WsRole::Server, Some(socket_config()))
                .await;
        let (stop_tx, mut stop) = watch::channel(false);
        let (_changed_tx, mut changed) = watch::channel(0);
        let task = tokio::spawn(async move { proxy_ws(a, b, &mut stop, &mut changed, 0).await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        stop_tx.send_replace(true);
        let outcome = tokio::time::timeout(Duration::from_millis(200), task).await;
        assert!(
            outcome.is_ok(),
            "stop could not interrupt a stalled heartbeat send"
        );
    }

    #[tokio::test]
    async fn slow_consumer_closes_bridge_instead_of_growing_buffer() {
        let (a_transport, a_peer) = tokio::io::duplex(1024);
        let (b_transport, _b_peer) = tokio::io::duplex(1024);
        let a =
            WebSocketStream::from_raw_socket(a_transport, WsRole::Server, Some(socket_config()))
                .await;
        let b =
            WebSocketStream::from_raw_socket(b_transport, WsRole::Server, Some(socket_config()))
                .await;
        let mut producer =
            WebSocketStream::from_raw_socket(a_peer, WsRole::Client, Some(socket_config())).await;
        let (_stop_tx, mut stop) = watch::channel(false);
        let (_changed_tx, mut changed) = watch::channel(0);
        let bridge = tokio::spawn(async move { proxy_ws(a, b, &mut stop, &mut changed, 0).await });
        let sending = tokio::spawn(async move {
            loop {
                if producer
                    .send(Message::Text("x".repeat(64 * 1024)))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let result = tokio::time::timeout(Duration::from_secs(7), bridge)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_err(),
            "slow peer must trigger bounded send timeout"
        );
        sending.abort();
    }

    #[tokio::test]
    async fn missing_host_heartbeat_expires_and_removes_route() {
        let (transport, _silent_peer) = tokio::io::duplex(4096);
        let ws = WebSocketStream::from_raw_socket(transport, WsRole::Server, Some(socket_config()))
            .await;
        let shared = test_shared();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            host_session(
                ws,
                shared.clone(),
                "fixture".into(),
                0,
                Duration::from_millis(20),
            ),
        )
        .await
        .unwrap();
        assert!(result.is_ok());
        assert!(shared.lock().hosts.is_empty());
    }
}
