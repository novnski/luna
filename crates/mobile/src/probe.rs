//! Direct-host descriptor probe (RM-00): the smallest real use of the
//! native-transport / Rust-core split.
//!
//! 1. The core validates a [`HostTicket`] before any network activity.
//! 2. It asks the platform [`HostTransport`] for one pinned GET. The transport
//!    accepts the connection only if the server key matches the pin.
//! 3. The transport reports back through [`DirectHostProbe::complete`] or
//!    [`DirectHostProbe::fail`], from any thread. The core checks the
//!    descriptor names the expected target, protocol and pin.
//!
//! Replies for anything but the current request are ignored, so a late reply
//! from a cancelled request can never change the state.

use std::sync::{Arc, Mutex, MutexGuard};

use luna_proto::remote_control::{
    DESCRIPTOR_PATH, HOST_TICKET_VERSION, HostTicket, REMOTE_DIRECT_V1, RemoteDescriptor,
    is_spki_sha256,
};

const MAX_TICKET_BYTES: usize = 4 * 1024;
const MAX_ENDPOINTS: usize = 8;
const MAX_DESCRIPTOR_BYTES: u32 = 16 * 1024;
const SUPPORTED_CATALOG_VERSIONS: &[u16] = &[1];

/// One pinned GET the platform must perform.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TransportRequest {
    pub request_id: u64,
    pub url: String,
    /// The only host name the TLS challenge may carry.
    pub host: String,
    /// Lowercase hex SHA-256 of the SubjectPublicKeyInfo the server must
    /// present.
    pub spki_sha256: String,
    pub max_body_bytes: u32,
}

/// The platform transport. Implementations report each request exactly once,
/// through `complete` or `fail`, from any thread (synchronously is fine).
#[uniffi::export(with_foreign)]
pub trait HostTransport: Send + Sync {
    fn get(&self, request: TransportRequest);
    fn cancel(&self, request_id: u64);
}

/// Observed route of a finished request, from the platform's own metrics.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RouteObservation {
    pub remote_address: Option<String>,
    pub local_address: Option<String>,
    /// `wifi`, `cellular`, `wired`, `loopback`, `other` or `unknown`.
    pub interface: String,
    pub proxied: bool,
    pub tls_version: Option<String>,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum TransportFailure {
    /// The server key did not match the pin, or trust evaluation failed.
    TrustRejected {
        detail: String,
    },
    Network {
        detail: String,
    },
    /// The body exceeded `max_body_bytes`; the transport stopped reading.
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RejectReason {
    TrustRejected,
    Network,
    HttpStatus,
    Body,
    UnsupportedVersion,
    PinMismatch,
    TargetMismatch,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ProbeState {
    Idle,
    Reading {
        request_id: u64,
        url: String,
    },
    Verified {
        device_id: String,
        environment_id: String,
        catalog_version: u16,
        url: String,
        route: RouteObservation,
    },
    Rejected {
        reason: RejectReason,
        detail: String,
        route: Option<RouteObservation>,
    },
    Cancelled,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ProbeError {
    #[error("invalid ticket: {detail}")]
    Ticket { detail: String },
    #[error("invalid endpoint: {detail}")]
    Endpoint { detail: String },
    #[error("a read is already in flight")]
    Busy,
}

/// State changes, delivered on whichever thread caused them.
#[uniffi::export(with_foreign)]
pub trait ProbeListener: Send + Sync {
    fn on_state(&self, state: ProbeState);
}

struct Endpoint {
    url: String,
    host: String,
}

struct Inner {
    next_request: u64,
    in_flight: Option<u64>,
    state: ProbeState,
}

#[derive(uniffi::Object)]
pub struct DirectHostProbe {
    ticket: HostTicket,
    endpoints: Vec<Endpoint>,
    transport: Arc<dyn HostTransport>,
    listener: Option<Arc<dyn ProbeListener>>,
    inner: Mutex<Inner>,
}

#[uniffi::export]
impl DirectHostProbe {
    /// Validate `ticket_json` completely before anything touches the network.
    #[uniffi::constructor]
    pub fn new(
        ticket_json: String,
        transport: Arc<dyn HostTransport>,
        listener: Option<Arc<dyn ProbeListener>>,
    ) -> Result<Arc<Self>, ProbeError> {
        let (ticket, endpoints) = parse_ticket(&ticket_json)?;
        Ok(Arc::new(Self {
            ticket,
            endpoints,
            transport,
            listener,
            inner: Mutex::new(Inner {
                next_request: 1,
                in_flight: None,
                state: ProbeState::Idle,
            }),
        }))
    }

    pub fn endpoints(&self) -> Vec<String> {
        self.ticket.endpoints.clone()
    }

    /// Ask the transport for the descriptor at endpoint `index`.
    pub fn read_descriptor(&self, index: u32) -> Result<u64, ProbeError> {
        let endpoint = self
            .endpoints
            .get(index as usize)
            .ok_or_else(|| ProbeError::Endpoint {
                detail: format!("no endpoint {index}"),
            })?;
        let request = {
            let mut inner = self.lock();
            if inner.in_flight.is_some() {
                return Err(ProbeError::Busy);
            }
            let request_id = inner.next_request;
            inner.next_request += 1;
            inner.in_flight = Some(request_id);
            inner.state = ProbeState::Reading {
                request_id,
                url: endpoint.url.clone(),
            };
            TransportRequest {
                request_id,
                url: endpoint.url.clone(),
                host: endpoint.host.clone(),
                spki_sha256: self.ticket.spki_sha256.clone(),
                max_body_bytes: MAX_DESCRIPTOR_BYTES,
            }
        };
        let request_id = request.request_id;
        self.notify();
        // Never call foreign code while holding the lock: the transport may
        // report back synchronously on this thread.
        self.transport.get(request);
        Ok(request_id)
    }

    /// The transport's reply for `request_id`.
    pub fn complete(&self, request_id: u64, status: u16, body: Vec<u8>, route: RouteObservation) {
        let Some(url) = self.take_current(request_id) else {
            return;
        };
        let state = self.evaluate(url, status, &body, route);
        self.settle(state);
    }

    /// The transport could not produce a reply for `request_id`.
    pub fn fail(
        &self,
        request_id: u64,
        failure: TransportFailure,
        route: Option<RouteObservation>,
    ) {
        if self.take_current(request_id).is_none() {
            return;
        }
        let (reason, detail) = match failure {
            TransportFailure::TrustRejected { detail } => (RejectReason::TrustRejected, detail),
            TransportFailure::Network { detail } => (RejectReason::Network, detail),
            TransportFailure::TooLarge => (
                RejectReason::Body,
                format!("descriptor is larger than {MAX_DESCRIPTOR_BYTES} bytes"),
            ),
        };
        self.settle(ProbeState::Rejected {
            reason,
            detail,
            route,
        });
    }

    /// Cancel the in-flight read. Its reply, if one still arrives, is ignored.
    pub fn cancel(&self) {
        let cancelled = {
            let mut inner = self.lock();
            let cancelled = inner.in_flight.take();
            if cancelled.is_some() {
                inner.state = ProbeState::Cancelled;
            }
            cancelled
        };
        if let Some(request_id) = cancelled {
            self.notify();
            self.transport.cancel(request_id);
        }
    }

    pub fn state(&self) -> ProbeState {
        self.lock().state.clone()
    }
}

impl DirectHostProbe {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claim the reply for `request_id` if it is still the current request.
    fn take_current(&self, request_id: u64) -> Option<String> {
        let mut inner = self.lock();
        if inner.in_flight != Some(request_id) {
            return None;
        }
        inner.in_flight = None;
        match &inner.state {
            ProbeState::Reading { url, .. } => Some(url.clone()),
            _ => None,
        }
    }

    fn evaluate(
        &self,
        url: String,
        status: u16,
        body: &[u8],
        route: RouteObservation,
    ) -> ProbeState {
        let reject = |reason, detail: String| ProbeState::Rejected {
            reason,
            detail,
            route: Some(route.clone()),
        };
        if status != 200 {
            return reject(RejectReason::HttpStatus, format!("HTTP status {status}"));
        }
        if body.len() > MAX_DESCRIPTOR_BYTES as usize {
            return reject(
                RejectReason::Body,
                format!("descriptor is larger than {MAX_DESCRIPTOR_BYTES} bytes"),
            );
        }
        let descriptor = match serde_json::from_slice::<RemoteDescriptor>(body) {
            Ok(descriptor) => descriptor,
            Err(error) => return reject(RejectReason::Body, format!("descriptor: {error}")),
        };
        if descriptor.protocol != REMOTE_DIRECT_V1
            || !SUPPORTED_CATALOG_VERSIONS.contains(&descriptor.catalog_version)
        {
            return reject(
                RejectReason::UnsupportedVersion,
                format!(
                    "protocol {} catalog {}",
                    descriptor.protocol, descriptor.catalog_version
                ),
            );
        }
        if descriptor.spki_sha256 != self.ticket.spki_sha256 {
            return reject(
                RejectReason::PinMismatch,
                "descriptor names a different key than the one checked".into(),
            );
        }
        if descriptor.target != self.ticket.target {
            return reject(
                RejectReason::TargetMismatch,
                format!(
                    "expected device {} environment {}, host is device {} environment {}",
                    self.ticket.target.device_id,
                    self.ticket.target.environment_id,
                    descriptor.target.device_id,
                    descriptor.target.environment_id
                ),
            );
        }
        ProbeState::Verified {
            device_id: descriptor.target.device_id,
            environment_id: descriptor.target.environment_id,
            catalog_version: descriptor.catalog_version,
            url,
            route,
        }
    }

    fn settle(&self, state: ProbeState) {
        self.lock().state = state;
        self.notify();
    }

    fn notify(&self) {
        if let Some(listener) = &self.listener {
            listener.on_state(self.state());
        }
    }
}

fn parse_ticket(json: &str) -> Result<(HostTicket, Vec<Endpoint>), ProbeError> {
    let ticket_error = |detail: String| ProbeError::Ticket { detail };
    if json.len() > MAX_TICKET_BYTES {
        return Err(ticket_error(format!(
            "ticket is larger than {MAX_TICKET_BYTES} bytes"
        )));
    }
    let ticket: HostTicket =
        serde_json::from_str(json).map_err(|error| ticket_error(error.to_string()))?;
    if ticket.version != HOST_TICKET_VERSION {
        return Err(ticket_error(format!(
            "unsupported ticket version {}",
            ticket.version
        )));
    }
    if !is_spki_sha256(&ticket.spki_sha256) {
        return Err(ticket_error("pin must be 64 lowercase hex digits".into()));
    }
    if ticket.endpoints.is_empty() || ticket.endpoints.len() > MAX_ENDPOINTS {
        return Err(ticket_error(format!(
            "ticket needs 1-{MAX_ENDPOINTS} endpoints"
        )));
    }
    let endpoints = ticket
        .endpoints
        .iter()
        .map(|endpoint| parse_endpoint(endpoint))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((ticket, endpoints))
}

/// Only a bare `https://host[:port]` origin is acceptable.
fn parse_endpoint(text: &str) -> Result<Endpoint, ProbeError> {
    let refuse = |why: &str| ProbeError::Endpoint {
        detail: format!("{text}: {why}"),
    };
    let url = url::Url::parse(text).map_err(|error| refuse(&error.to_string()))?;
    if url.scheme() != "https" {
        return Err(refuse("only https is allowed"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refuse("credentials are not allowed in an endpoint"));
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err(refuse(
            "an endpoint is an origin; no path, query or fragment",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| refuse("missing host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    Ok(Endpoint {
        url: format!("{}{DESCRIPTOR_PATH}", url.origin().ascii_serialization()),
        host,
    })
}
