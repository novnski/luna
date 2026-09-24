use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use url::Url;

use crate::protocol::{
    Control, HANDSHAKE_SECS, HEARTBEAT_SECS, MAX_ACTIVE_PER_ROUTE, Target, VERSION, proof,
    validate_id,
};
use crate::server::{proxy_ws, recv_control, send_control, socket_config};

type Outbound = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
struct AdmissionRejected;

impl std::fmt::Display for AdmissionRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("relay credential, version or target rejected")
    }
}

impl std::error::Error for AdmissionRejected {}

struct BridgeArgs {
    base: String,
    route: String,
    stream_id: String,
    target: Target,
    secret: String,
    generation: u64,
    guarded_rpc_port: u16,
    stop: watch::Receiver<bool>,
}

fn route_url(base: &str, path: &str) -> Result<String> {
    let mut url = Url::parse(base).context("parse relay URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!("relay URL must be an origin without credentials, path or query");
    }
    match url.scheme() {
        "wss" => {}
        "ws" if url
            .host_str()
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback()) => {}
        _ => bail!("relay requires WSS, except loopback WS for local testing"),
    }
    url.set_path(path);
    Ok(url.into())
}

async fn dial(
    base: &str,
    path: &str,
    target: &Target,
    secret: &str,
    client_id: Option<&str>,
    generation: u64,
) -> Result<(Outbound, u64)> {
    let url = route_url(base, path)?;
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_SECS),
        tokio_tungstenite::connect_async_with_config(url, Some(socket_config()), false),
    )
    .await
    .context("relay dial timeout")??;
    let Control::Challenge { nonce } =
        tokio::time::timeout(Duration::from_secs(HANDSHAKE_SECS), recv_control(&mut ws))
            .await
            .context("relay challenge timeout")??
    else {
        bail!("relay did not challenge");
    };
    send_control(
        &mut ws,
        &Control::Hello {
            version: VERSION,
            target: target.clone(),
            client_id: client_id.map(str::to_owned),
            generation,
            proof: proof(secret, &nonce, path, target, generation)?,
        },
    )
    .await?;
    match tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_SECS + 8),
        recv_control(&mut ws),
    )
    .await
    .context("relay admission timeout")??
    {
        Control::Ready { generation } => Ok((ws, generation)),
        Control::Rejected { .. } => Err(AdmissionRejected.into()),
        _ => bail!("unexpected relay admission reply"),
    }
}

pub async fn client_socket(
    base: &str,
    route: &str,
    target: &Target,
    client_id: &str,
    secret: &str,
) -> Result<Outbound> {
    validate_id(route)?;
    validate_id(client_id)?;
    let path = format!("/v1/client/{route}/{client_id}");
    let (ws, _) = dial(base, &path, target, secret, Some(client_id), 0).await?;
    Ok(ws)
}

/// The local forwarding listener is only an RPC route. It never receives a
/// local-owner service or grants a principal to its callers.
pub async fn run_client_forward(
    listener: TcpListener,
    base: String,
    route: String,
    target: Target,
    client_id: String,
    secret: String,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        bail!("client RPC forward must bind loopback");
    }
    validate_id(&route)?;
    validate_id(&client_id)?;
    let slots = std::sync::Arc::new(Semaphore::new(MAX_ACTIVE_PER_ROUTE));
    let (stop_tx, _) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                let (base, route, target, client_id, secret) = (
                    base.clone(), route.clone(), target.clone(), client_id.clone(), secret.clone());
                let stop = stop_tx.subscribe();
                tasks.spawn(async move {
                    let _slot = slot;
                    if let Err(error) = forward_one(stream, &base, &route, &target, &client_id, &secret, stop).await {
                        tracing::debug!(%error, "client forward closed");
                    }
                });
            }
        }
    }
    stop_tx.send_replace(true);
    tasks.shutdown().await;
    Ok(())
}

async fn forward_one(
    stream: TcpStream,
    base: &str,
    route: &str,
    target: &Target,
    client_id: &str,
    secret: &str,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    #[allow(clippy::result_large_err)]
    let callback =
        |req: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
            if req.headers().contains_key("origin")
                || req.uri().path() != "/"
                || req.uri().query().is_some()
            {
                let mut rejection = ErrorResponse::new(Some("local route not allowed".into()));
                *rejection.status_mut() = StatusCode::FORBIDDEN;
                return Err(rejection);
            }
            Ok(response)
        };
    let local = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_SECS),
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(socket_config())),
    )
    .await??;
    let remote = client_socket(base, route, target, client_id, secret).await?;
    let (_changed_tx, mut changed) = watch::channel(0);
    proxy_ws(local, remote, &mut stop, &mut changed).await
}

/// The host is a separate sidecar beside a running headless engine. The only
/// local destination is this explicit guarded loopback remote-RPC port.
pub async fn run_host(
    base: &str,
    route: &str,
    target: &Target,
    host_secret: &str,
    guarded_rpc_port: u16,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    validate_id(route)?;
    if guarded_rpc_port == 0 {
        bail!("guarded RPC port is required");
    }
    let probe = connect_guarded_local(guarded_rpc_port).await?;
    drop(probe);
    let mut failures = 0_u32;
    while !*stop.borrow() {
        let connected_at = Instant::now();
        let result = host_once(
            base,
            route,
            target,
            host_secret,
            guarded_rpc_port,
            stop.clone(),
        )
        .await;
        if *stop.borrow() {
            return Ok(());
        }
        if result
            .as_ref()
            .is_err_and(|error| error.is::<AdmissionRejected>())
        {
            bail!("relay host admission rejected; rotate or correct credentials");
        }
        if connected_at.elapsed() > Duration::from_secs(60) {
            failures = 0;
        }
        failures += 1;
        if failures >= 8 {
            return Err(anyhow::anyhow!(
                "relay host disconnected after 8 attempts: {result:?}"
            ));
        }
        let delay = Duration::from_secs(1_u64 << failures.min(5));
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            _ = tokio::time::sleep(delay) => {},
        }
    }
    Ok(())
}

async fn host_once(
    base: &str,
    route: &str,
    target: &Target,
    host_secret: &str,
    guarded_rpc_port: u16,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let path = format!("/v1/host/{route}");
    let (ws, generation) = dial(base, &path, target, host_secret, None, 0).await?;
    let (mut output, mut input) = ws.split();
    let (bridge_stop, _) = watch::channel(false);
    let slots = std::sync::Arc::new(Semaphore::new(MAX_ACTIVE_PER_ROUTE));
    let mut tasks = tokio::task::JoinSet::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = Instant::now();
    let result = loop {
        tokio::select! {
            _ = stop.changed() => break Ok(()),
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            message = input.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let Control::Open { stream_id, generation: requested } = serde_json::from_str(&text)? else {
                        break Err(anyhow::anyhow!("unexpected host control"));
                    };
                    if requested != generation || validate_id(&stream_id).is_err() { continue; }
                    let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                    let args = BridgeArgs {
                        base: base.to_owned(), route: route.to_owned(), stream_id,
                        target: target.clone(), secret: host_secret.to_owned(),
                        generation: requested, guarded_rpc_port, stop: bridge_stop.subscribe(),
                    };
                    tasks.spawn(async move {
                        let _slot = slot;
                        let _ = bridge_one(args).await;
                    });
                }
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Ping(payload))) => { output.send(Message::Pong(payload)).await?; },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break Ok(()),
                _ => break Err(anyhow::anyhow!("unexpected host frame")),
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(3 * HEARTBEAT_SECS) { break Ok(()); }
                output.send(Message::Ping(Vec::new())).await?;
            }
        }
    };
    bridge_stop.send_replace(true);
    tasks.shutdown().await;
    result
}

async fn bridge_one(mut args: BridgeArgs) -> Result<()> {
    let local = connect_guarded_local(args.guarded_rpc_port).await?;
    let path = format!("/v1/bridge/{}/{}", args.route, args.stream_id);
    let (remote, admitted_generation) = dial(
        &args.base,
        &path,
        &args.target,
        &args.secret,
        None,
        args.generation,
    )
    .await?;
    if admitted_generation != args.generation {
        bail!("bridge generation changed");
    }
    let (_changed_tx, mut changed) = watch::channel(0);
    proxy_ws(local, remote, &mut args.stop, &mut changed).await
}

async fn connect_guarded_local(port: u16) -> Result<Outbound> {
    let url = format!("ws://127.0.0.1:{port}");
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_SECS),
        tokio_tungstenite::connect_async_with_config(url, Some(socket_config()), false),
    )
    .await
    .context("guarded local RPC unavailable")??;
    ws.send(Message::Text(
        r#"{"id":0,"method":"EngineInfo","params":{}}"#.into(),
    ))
    .await?;
    let reply = tokio::time::timeout(Duration::from_secs(HANDSHAKE_SECS), ws.next())
        .await
        .context("guarded RPC probe timed out")?
        .context("guarded RPC probe closed")??;
    let Message::Text(text) = reply else {
        bail!("guarded RPC probe returned nontext");
    };
    let value: serde_json::Value = serde_json::from_str(&text)?;
    if value["id"] != 0
        || !value["err"]
            .as_str()
            .is_some_and(|error| error.contains("device session required"))
    {
        bail!("local port is not a L29 guarded remote RPC listener");
    }
    Ok(ws)
}
