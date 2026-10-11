//! The M0 direct listener: explicit interface, TLS only, two harmless routes.
//!
//! `GET /remote/v1/health` and `GET /remote/v1/descriptor` are the only
//! routes. Other paths are 404, other methods 405, and any request body is
//! refused before it is read. Nothing is proxied to local IPC.

use std::convert::Infallible;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use luna_proto::remote_control::{
    DESCRIPTOR_PATH, HEALTH_PATH, REMOTE_DIRECT_V1, RemoteDescriptor,
};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("refusing wildcard address {0}: bind one explicit interface")]
    Wildcard(IpAddr),
    #[error("refusing loopback address {0}: it is not a remote route")]
    Loopback(IpAddr),
    #[error("an explicit, nonzero port is required")]
    NoPort,
    #[error(
        "port {0} is already in use on loopback (for example by LUNA's local IPC); choose another"
    )]
    PortInUseOnLoopback(u16),
    #[error("{0} is already in use")]
    InUse(SocketAddr),
    #[error("binding {0} failed: {1}")]
    Io(SocketAddr, std::io::Error),
}

/// Bind the direct listener. The port must also be free on loopback, so a
/// remote listener never shares a port number with the local-owner IPC.
pub async fn bind(addr: SocketAddr) -> Result<TcpListener, BindError> {
    let ip = addr.ip();
    if ip.is_unspecified() {
        return Err(BindError::Wildcard(ip));
    }
    if ip.is_loopback() {
        return Err(BindError::Loopback(ip));
    }
    if addr.port() == 0 {
        return Err(BindError::NoPort);
    }
    let loopback: IpAddr = match ip {
        IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
        IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
    };
    match std::net::TcpListener::bind((loopback, addr.port())) {
        Ok(probe) => drop(probe),
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            return Err(BindError::PortInUseOnLoopback(addr.port()));
        }
        Err(error) => return Err(BindError::Io(addr, error)),
    }
    TcpListener::bind(addr)
        .await
        .map_err(|error| match error.kind() {
            ErrorKind::AddrInUse => BindError::InUse(addr),
            _ => BindError::Io(addr, error),
        })
}

/// What the listener observed, for the operator's log.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum HostEvent {
    #[serde(rename_all = "camelCase")]
    Handshake {
        peer: SocketAddr,
        ok: bool,
        tls_version: Option<String>,
        alpn: Option<String>,
        error: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Request {
        peer: SocketAddr,
        method: String,
        path: String,
        status: u16,
    },
    #[serde(rename_all = "camelCase")]
    Closed {
        peer: SocketAddr,
        error: Option<String>,
    },
}

/// Serve the descriptor until the listener fails or the task is dropped.
pub async fn serve(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    descriptor: &RemoteDescriptor,
    events: Arc<dyn Fn(HostEvent) + Send + Sync>,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(config);
    let routes = Arc::new(Routes {
        descriptor: Bytes::from(serde_json::to_vec(descriptor).expect("descriptor serializes")),
        health: Bytes::from(
            serde_json::to_vec(&serde_json::json!({ "protocol": REMOTE_DIRECT_V1 }))
                .expect("health serializes"),
        ),
    });
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (tcp, peer) = listener.accept().await?;
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            // Over the bound: close instead of queueing without limit.
            drop(tcp);
            continue;
        };
        let (acceptor, routes, events) = (acceptor.clone(), routes.clone(), events.clone());
        tokio::spawn(async move {
            let _slot = slot;
            let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                Ok(Ok(tls)) => tls,
                Ok(Err(error)) => {
                    events(handshake_failed(peer, error.to_string()));
                    return;
                }
                Err(_) => {
                    events(handshake_failed(peer, "handshake timed out".into()));
                    return;
                }
            };
            let (_, session) = tls.get_ref();
            events(HostEvent::Handshake {
                peer,
                ok: true,
                tls_version: session.protocol_version().map(|v| format!("{v:?}")),
                alpn: session
                    .alpn_protocol()
                    .map(|p| String::from_utf8_lossy(p).into_owned()),
                error: None,
            });
            let service_events = events.clone();
            let service = hyper::service::service_fn(move |request| {
                let (routes, events) = (routes.clone(), service_events.clone());
                async move {
                    let method = request.method().to_string();
                    let path = request.uri().path().to_owned();
                    let response = routes.respond(&request);
                    events(HostEvent::Request {
                        peer,
                        method,
                        path,
                        status: response.status().as_u16(),
                    });
                    Ok::<_, Infallible>(response)
                }
            });
            let result = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .max_buf_size(MAX_HEADER_BYTES)
                .serve_connection(TokioIo::new(tls), service)
                .await;
            events(HostEvent::Closed {
                peer,
                error: result.err().map(|error| error.to_string()),
            });
        });
    }
}

fn handshake_failed(peer: SocketAddr, error: String) -> HostEvent {
    HostEvent::Handshake {
        peer,
        ok: false,
        tls_version: None,
        alpn: None,
        error: Some(error),
    }
}

struct Routes {
    descriptor: Bytes,
    health: Bytes,
}

impl Routes {
    fn respond(&self, request: &Request<Incoming>) -> Response<Full<Bytes>> {
        let has_body = request.headers().contains_key(TRANSFER_ENCODING)
            || request
                .headers()
                .get(CONTENT_LENGTH)
                .is_some_and(|length| length.as_bytes() != b"0");
        if has_body {
            return reply(StatusCode::PAYLOAD_TOO_LARGE, Bytes::new());
        }
        let route = match request.uri().path() {
            HEALTH_PATH => &self.health,
            DESCRIPTOR_PATH => &self.descriptor,
            _ => return reply(StatusCode::NOT_FOUND, Bytes::new()),
        };
        if request.method() != Method::GET {
            let mut response = reply(StatusCode::METHOD_NOT_ALLOWED, Bytes::new());
            response
                .headers_mut()
                .insert(ALLOW, "GET".parse().expect("static header"));
            return response;
        }
        reply(StatusCode::OK, route.clone())
    }
}

fn reply(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, "no-store".parse().expect("static header"));
    if status == StatusCode::OK {
        headers.insert(
            CONTENT_TYPE,
            "application/json".parse().expect("static header"),
        );
    }
    response
}
