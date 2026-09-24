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
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::protocol::{
    BRIDGE_SECS, Control, HANDSHAKE_SECS, HEARTBEAT_SECS, MAX_ACTIVE_PER_ROUTE, MAX_MESSAGE,
    RelayConfig, VERSION, proof, proof_matches, random_hex, validate_id,
};

type Socket = WebSocketStream<TcpStream>;

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
        let count = state.active.entry(self.route.clone()).or_default();
        *count = count.saturating_sub(1);
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
    let accepted = {
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
        key.zip(route).is_some_and(|(key, route)| {
            version == VERSION
                && target == route.target
                && right_client
                && right_generation
                && proof(key, &nonce, &path, &target, generation)
                    .is_ok_and(|expected| proof_matches(&expected, &supplied))
        })
    };
    if !accepted {
        let _ = send_control(
            &mut ws,
            &Control::Rejected {
                reason: "credential, version or target rejected".into(),
            },
        )
        .await;
        bail!("relay admission rejected");
    }
    match role {
        Role::Host(route) => {
            host_session(ws, shared, route, Duration::from_secs(HEARTBEAT_SECS)).await
        }
        Role::Client(route, id) => client_session(ws, shared, route, id).await,
        Role::Bridge(route, stream) => bridge_session(ws, shared, route, stream, generation).await,
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
    heartbeat_period: Duration,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (tx, mut rx) = mpsc::channel(32);
    let (stop, mut stopping) = watch::channel(false);
    let generation = {
        let mut state = shared.lock();
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
    if let Err(error) = send_control(&mut ws, &Control::Ready { generation }).await {
        let mut state = shared.lock();
        if state
            .hosts
            .get(&route)
            .is_some_and(|host| host.generation == generation)
        {
            state.hosts.remove(&route);
        }
        return Err(error);
    }
    let (mut sink, mut input) = ws.split();
    let mut heartbeat = tokio::time::interval(heartbeat_period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();
    let result = loop {
        tokio::select! {
            _ = stopping.changed() => break Ok(()),
            message = rx.recv() => match message {
                Some(control) => {
                    let text = serde_json::to_string(&control)?;
                    if !matches!(tokio::time::timeout(Duration::from_secs(5), sink.send(Message::Text(text))).await, Ok(Ok(()))) { break Ok(()); }
                }
                None => break Ok(()),
            },
            message = input.next() => match message {
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break Ok(()),
                Some(Ok(Message::Ping(payload))) => { sink.send(Message::Pong(payload)).await?; },
                Some(Ok(_)) => break Err(anyhow::anyhow!("unexpected host control frame")),
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > heartbeat_period * 3 { break Ok(()); }
                if sink.send(Message::Ping(Vec::new())).await.is_err() { break Ok(()); }
            }
        }
    };
    let mut state = shared.lock();
    if state
        .hosts
        .get(&route)
        .is_some_and(|host| host.generation == generation)
    {
        state.hosts.remove(&route);
        state
            .pending
            .retain(|_, pending| pending.route != route || pending.generation != generation);
    }
    result
}

async fn client_session(
    mut ws: Socket,
    shared: Arc<Shared>,
    route: String,
    _id: String,
) -> Result<()> {
    let stream_id = random_hex()?;
    let (tx, rx) = oneshot::channel();
    let (host, _reservation) = {
        let mut state = shared.lock();
        let host = state
            .hosts
            .get(&route)
            .cloned()
            .context("environment offline")?;
        let count = state.active.entry(route.clone()).or_default();
        if *count >= MAX_ACTIVE_PER_ROUTE {
            bail!("route connection limit reached");
        }
        *count += 1;
        state.pending.insert(
            stream_id.clone(),
            Pending {
                route: route.clone(),
                generation: host.generation,
                deliver: tx,
            },
        );
        (
            host,
            Reservation {
                shared: shared.clone(),
                route: route.clone(),
                stream_id: stream_id.clone(),
            },
        )
    };
    host.tx
        .try_send(Control::Open {
            stream_id,
            generation: host.generation,
        })
        .map_err(|_| anyhow::anyhow!("host control queue full or closed"))?;
    let mut host_stop = host.stop.subscribe();
    let mut changed = shared.changed.subscribe();
    let bridge = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(BRIDGE_SECS), rx) => result.context("bridge timed out")??,
        _ = host_stop.changed() => bail!("host generation changed"),
        _ = changed.changed() => bail!("relay configuration changed"),
    };
    send_control(
        &mut ws,
        &Control::Ready {
            generation: host.generation,
        },
    )
    .await?;
    proxy_ws(ws, bridge, &mut host_stop, &mut changed).await
}

async fn bridge_session(
    mut ws: Socket,
    shared: Arc<Shared>,
    route: String,
    stream: String,
    generation: u64,
) -> Result<()> {
    let pending = {
        let mut state = shared.lock();
        if !state
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
        state
            .pending
            .remove(&stream)
            .context("bridge disappeared")?
    };
    send_control(&mut ws, &Control::Ready { generation }).await?;
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
) -> Result<()>
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut a_out, mut a_in) = a.split();
    let (mut b_out, mut b_in) = b.split();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut a_pong = Instant::now();
    let mut b_pong = Instant::now();
    loop {
        tokio::select! {
            _ = host_stop.changed() => break,
            _ = changed.changed() => break,
            message = a_in.next() => match message {
                Some(Ok(Message::Text(text))) if text.len() <= MAX_MESSAGE => {
                    tokio::time::timeout(Duration::from_secs(5), b_out.send(Message::Text(text))).await??;
                }
                Some(Ok(Message::Pong(_))) => a_pong = Instant::now(),
                Some(Ok(Message::Ping(payload))) => { a_out.send(Message::Pong(payload)).await?; },
                _ => break,
            },
            message = b_in.next() => match message {
                Some(Ok(Message::Text(text))) if text.len() <= MAX_MESSAGE => {
                    tokio::time::timeout(Duration::from_secs(5), a_out.send(Message::Text(text))).await??;
                }
                Some(Ok(Message::Pong(_))) => b_pong = Instant::now(),
                Some(Ok(Message::Ping(payload))) => { b_out.send(Message::Pong(payload)).await?; },
                _ => break,
            },
            _ = heartbeat.tick() => {
                if a_pong.elapsed() > Duration::from_secs(3 * HEARTBEAT_SECS)
                    || b_pong.elapsed() > Duration::from_secs(3 * HEARTBEAT_SECS) { break; }
                a_out.send(Message::Ping(Vec::new())).await?;
                b_out.send(Message::Ping(Vec::new())).await?;
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), a_out.send(Message::Close(None))).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), b_out.send(Message::Close(None))).await;
    Ok(())
}

pub async fn send_control<S>(ws: &mut WebSocketStream<S>, message: &Control) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    ws.send(Message::Text(serde_json::to_string(message)?))
        .await?;
    Ok(())
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
    use tokio_tungstenite::tungstenite::protocol::Role as WsRole;

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
        let bridge = tokio::spawn(async move { proxy_ws(a, b, &mut stop, &mut changed).await });
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
        let (changed, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                config: None,
                revision: 0,
                next_generation: 0,
                hosts: HashMap::new(),
                pending: HashMap::new(),
                active: HashMap::new(),
            }),
            changed,
        });
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            host_session(
                ws,
                shared.clone(),
                "fixture".into(),
                Duration::from_millis(20),
            ),
        )
        .await
        .unwrap();
        assert!(result.is_ok());
        assert!(shared.lock().hosts.is_empty());
    }
}
