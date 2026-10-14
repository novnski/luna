//! RM-02 enrollment owner. Swift supplies pinned transport and device-only
//! Keychain operations; this core owns their meaning and request generations.
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use luna_proto::remote_control::{HostTicket, PAIR_PATH, PairingTicket, SESSION_CHECK_PATH};
use serde_json::{Value, json};

use crate::probe::{RouteObservation, TransportFailure, parse_ticket, verify_descriptor};

#[derive(Clone, uniffi::Record)]
pub struct EnrollmentRequest {
    pub request_id: u64,
    pub url: String,
    pub host: String,
    pub spki_sha256: String,
    pub method: String,
    pub body: Vec<u8>,
    pub authorization: Option<String>,
    pub max_body_bytes: u32,
}

/// Secret-bearing platform persistence input. Never serialize this record to
/// preferences, debug output, screenshots or diagnostics.
#[derive(Clone, uniffi::Record)]
pub struct SavedMac {
    pub host_name: String,
    pub host_ticket: String,
    pub session_id: String,
    pub session_token: String,
    pub expires_at_ms: u64,
    pub scopes: Vec<String>,
    pub client_label: String,
}

#[derive(Clone, uniffi::Record)]
pub struct PairingPreview {
    pub host_name: String,
    pub device_id: String,
    pub environment_id: String,
    pub workspace_scope: String,
    pub endpoints: Vec<String>,
    pub spki_sha256: String,
    pub expires_at_ms: u64,
    pub scopes: Vec<String>,
}

#[derive(Clone, uniffi::Enum)]
pub enum EnrollmentAction {
    Request {
        request: EnrollmentRequest,
    },
    Save {
        request_id: u64,
        mac: SavedMac,
    },
    Connected {
        session_id: String,
        route: RouteObservation,
    },
    Rejected {
        reason: String,
    },
    RePair {
        reason: String,
    },
    Uncertain {
        reason: String,
    },
    Ignore,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EnrollmentError {
    #[error("Invalid or unsupported pairing code")]
    InvalidCode,
    #[error("Invalid or unsafe host identity/route")]
    InvalidHost,
    #[error("Pairing code has expired; create a new code on the Mac")]
    Expired,
    #[error("Invalid saved session; pair again")]
    InvalidSession,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Descriptor,
    Redeem,
    Saving,
    Authenticate,
}

struct Pending {
    id: u64,
    stage: Stage,
    origin: String,
    label: String,
}
struct Inner {
    next_id: u64,
    pending: Option<Pending>,
    session: Option<SavedMac>,
    attempted: bool,
    phase: String,
}

#[derive(uniffi::Object)]
pub struct Enrollment {
    host: HostTicket,
    ticket: Option<PairingTicket>,
    inner: Mutex<Inner>,
}

#[uniffi::export]
impl Enrollment {
    #[uniffi::constructor]
    pub fn new(code: String) -> Result<Arc<Self>, EnrollmentError> {
        let ticket = PairingTicket::decode(&code).map_err(|_| EnrollmentError::InvalidCode)?;
        if ticket.version != 1
            || ticket.pairing_id.is_empty()
            || ticket.pairing_id.len() > 128
            || !secret(&ticket.credential)
            || !scopes_valid(&ticket.scopes)
        {
            return Err(EnrollmentError::InvalidCode);
        }
        if ticket.expires_at_ms <= now() {
            return Err(EnrollmentError::Expired);
        }
        if ticket.expires_at_ms > now().saturating_add(600_000) {
            return Err(EnrollmentError::InvalidCode);
        }
        validate_host(&ticket.host)?;
        Ok(Self::assemble(ticket.host.clone(), Some(ticket), None))
    }

    /// Manual route entry changes only the route. Identity/pin/scopes still
    /// come from the out-of-band code and use the same enrollment validator.
    #[uniffi::constructor]
    pub fn manual(code: String, endpoint: String) -> Result<Arc<Self>, EnrollmentError> {
        let mut ticket = PairingTicket::decode(&code).map_err(|_| EnrollmentError::InvalidCode)?;
        ticket.host.endpoints = vec![endpoint];
        Self::new(ticket.encode().map_err(|_| EnrollmentError::InvalidCode)?)
    }

    #[uniffi::constructor]
    pub fn restore(mac: SavedMac) -> Result<Arc<Self>, EnrollmentError> {
        let host = validate_saved(&mac)?;
        Ok(Self::assemble(host, None, Some(mac)))
    }

    pub fn preview(&self) -> PairingPreview {
        PairingPreview {
            host_name: self
                .ticket
                .as_ref()
                .and_then(|t| t.host_name.clone())
                .unwrap_or_else(|| {
                    self.lock()
                        .session
                        .as_ref()
                        .map(|m| m.host_name.clone())
                        .unwrap_or_else(|| "Paired Mac".into())
                }),
            device_id: self.host.target.device_id.clone(),
            environment_id: self.host.target.environment_id.clone(),
            workspace_scope: serde_json::to_value(self.host.target.workspace_scope)
                .expect("scope serializes")
                .as_str()
                .unwrap_or_default()
                .into(),
            endpoints: self.host.endpoints.clone(),
            spki_sha256: self.host.spki_sha256.clone(),
            expires_at_ms: self
                .ticket
                .as_ref()
                .map(|t| t.expires_at_ms)
                .unwrap_or_else(|| {
                    self.lock()
                        .session
                        .as_ref()
                        .map(|m| m.expires_at_ms)
                        .unwrap_or(0)
                }),
            scopes: self
                .ticket
                .as_ref()
                .map(|t| t.scopes.clone())
                .unwrap_or_else(|| {
                    self.lock()
                        .session
                        .as_ref()
                        .map(|m| m.scopes.clone())
                        .unwrap_or_default()
                }),
        }
    }

    pub fn phase(&self) -> String {
        self.lock().phase.clone()
    }

    /// A preflight failure can be retried after fixing permission or routing.
    /// Once redemption starts, only a fresh code can resolve uncertainty.
    pub fn retry_available(&self) -> bool {
        let inner = self.lock();
        self.ticket
            .as_ref()
            .is_some_and(|ticket| ticket.expires_at_ms > now())
            && !inner.attempted
            && inner.pending.is_none()
    }

    /// Called only after explicit native confirmation (or foregrounding an
    /// already saved host). Every attempt first revalidates the pinned host.
    pub fn start(&self, endpoint_index: u32, client_label: String) -> EnrollmentAction {
        let mut inner = self.lock();
        if inner.pending.is_some() {
            return EnrollmentAction::Ignore;
        }
        if self.ticket.is_some() && inner.attempted {
            return rejected(
                &mut inner,
                "Use a fresh pairing code after redemption was attempted",
            );
        }
        if self
            .ticket
            .as_ref()
            .is_some_and(|t| t.expires_at_ms <= now())
        {
            return rejected(
                &mut inner,
                "Pairing code has expired; create a new code on the Mac",
            );
        }
        if inner
            .session
            .as_ref()
            .is_some_and(|m| m.expires_at_ms <= now())
        {
            inner.session = None;
            inner.phase = "re-pair".into();
            return EnrollmentAction::RePair {
                reason: "Access has expired; pair again on the Mac".into(),
            };
        }
        if client_label.trim().is_empty() || client_label.len() > 100 {
            return rejected(&mut inner, "Phone name must be 1–100 bytes");
        }
        let Some(origin) = self.host.endpoints.get(endpoint_index as usize).cloned() else {
            return rejected(&mut inner, "Choose an endpoint supplied by this Mac");
        };
        let origin = origin.trim_end_matches('/').to_owned();
        let request = self.request(
            &mut inner,
            Stage::Descriptor,
            origin,
            client_label,
            Vec::new(),
            None,
        );
        EnrollmentAction::Request { request }
    }

    pub fn complete(
        &self,
        request_id: u64,
        status: u16,
        body: Vec<u8>,
        route: RouteObservation,
    ) -> EnrollmentAction {
        let mut inner = self.lock();
        if inner.pending.as_ref().map(|p| p.id) != Some(request_id) {
            return EnrollmentAction::Ignore;
        }
        let pending = inner.pending.take().expect("current request");
        if pending.stage == Stage::Saving {
            inner.pending = Some(pending);
            return EnrollmentAction::Ignore;
        }
        if body.len() > 16 * 1024 {
            if pending.stage == Stage::Redeem {
                inner.phase = "uncertain".into();
                return EnrollmentAction::Uncertain {
                    reason: "Pairing reply exceeded the limit; check the Mac's sessions and use a fresh code".into(),
                };
            }
            return rejected(&mut inner, "Host reply exceeded the size limit");
        }
        match pending.stage {
            Stage::Descriptor => {
                if verify_descriptor(&self.host, status, &body).is_err() {
                    return rejected(
                        &mut inner,
                        "Host identity, pin or protocol does not match this pairing",
                    );
                }
                if let Some(ticket) = &self.ticket {
                    if ticket.expires_at_ms <= now() {
                        return rejected(&mut inner, "Pairing code has expired");
                    }
                    inner.attempted = true;
                    let bytes = serde_json::to_vec(&json!({"target": self.host.target,
                        "credential": ticket.credential, "clientLabel": pending.label}))
                    .expect("request serializes");
                    let request = self.request(
                        &mut inner,
                        Stage::Redeem,
                        pending.origin,
                        pending.label,
                        bytes,
                        None,
                    );
                    EnrollmentAction::Request { request }
                } else {
                    self.authenticate(&mut inner, pending.origin, pending.label)
                }
            }
            Stage::Redeem => {
                if status == 403 {
                    return rejected(
                        &mut inner,
                        "Pairing was expired, cancelled or already used; create a fresh code",
                    );
                }
                if status != 200 {
                    inner.phase = "uncertain".into();
                    return EnrollmentAction::Uncertain {reason: "The Mac may have consumed this code; check its paired sessions and create a fresh code".into()};
                }
                let Some(mac) = self.session_reply(&body, &pending.label) else {
                    inner.phase = "uncertain".into();
                    return EnrollmentAction::Uncertain {
                        reason: "Pairing reply was invalid; check the Mac and create a fresh code"
                            .into(),
                    };
                };
                inner.session = Some(mac.clone());
                inner.phase = "saving".into();
                inner.pending = Some(Pending {
                    stage: Stage::Saving,
                    ..pending
                });
                EnrollmentAction::Save { request_id, mac }
            }
            Stage::Authenticate => {
                if status == 403 {
                    inner.session = None;
                    inner.phase = "re-pair".into();
                    return EnrollmentAction::RePair {
                        reason: "Access was revoked or expired; pair again on the Mac".into(),
                    };
                }
                let value: Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(_) => return rejected(&mut inner, "Invalid authenticated host reply"),
                };
                let Some(mac) = &inner.session else {
                    return rejected(&mut inner, "Session is unavailable");
                };
                if status != 200
                    || value["id"].as_str() != Some(&mac.session_id)
                    || value["revoked"] != false
                    || value["expiresAtMs"].as_u64() != Some(mac.expires_at_ms)
                    || serde_json::from_value::<Vec<String>>(value["scopes"].clone())
                        .ok()
                        .as_ref()
                        != Some(&mac.scopes)
                {
                    return rejected(
                        &mut inner,
                        "Authenticated session does not match the saved grant",
                    );
                }
                let session_id = mac.session_id.clone();
                inner.phase = "connected".into();
                EnrollmentAction::Connected { session_id, route }
            }
            Stage::Saving => unreachable!(),
        }
    }

    pub fn persisted(&self, request_id: u64, success: bool) -> EnrollmentAction {
        let mut inner = self.lock();
        if !inner
            .pending
            .as_ref()
            .is_some_and(|p| p.id == request_id && p.stage == Stage::Saving)
        {
            return EnrollmentAction::Ignore;
        }
        let pending = inner.pending.take().expect("current save");
        if !success {
            inner.session = None;
            inner.phase = "re-pair".into();
            return EnrollmentAction::RePair {reason: "The phone could not save access securely; revoke the unused session on the Mac and create a fresh code".into()};
        }
        self.authenticate(&mut inner, pending.origin, pending.label)
    }

    pub fn fail(&self, request_id: u64, failure: TransportFailure) -> EnrollmentAction {
        let mut inner = self.lock();
        if inner.pending.as_ref().map(|p| p.id) != Some(request_id) {
            return EnrollmentAction::Ignore;
        }
        if inner
            .pending
            .as_ref()
            .is_some_and(|p| p.stage == Stage::Saving)
        {
            return EnrollmentAction::Ignore;
        }
        let pending = inner.pending.take().expect("current request");
        if pending.stage == Stage::Redeem
            && matches!(
                failure,
                TransportFailure::Network { .. } | TransportFailure::TooLarge
            )
        {
            inner.phase = "uncertain".into();
            EnrollmentAction::Uncertain {reason: "The pairing reply was lost; the Mac may have consumed the code. Check its sessions and use a fresh code".into()}
        } else {
            rejected(
                &mut inner,
                match failure {
                    TransportFailure::TrustRejected { .. } => {
                        "TLS trust was refused; check the Mac's pairing code"
                    }
                    TransportFailure::Network { .. } => {
                        "The Mac is unreachable. Check local-network permission and the selected route"
                    }
                    TransportFailure::TooLarge => "Host reply exceeded the size limit",
                },
            )
        }
    }

    pub fn cancel(&self) -> Option<u64> {
        let mut inner = self.lock();
        let pending = inner.pending.take();
        inner.phase = if inner.attempted && self.ticket.is_some() {
            "uncertain"
        } else {
            "cancelled"
        }
        .into();
        pending.map(|p| p.id)
    }
}

impl Enrollment {
    fn assemble(
        host: HostTicket,
        ticket: Option<PairingTicket>,
        session: Option<SavedMac>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            ticket,
            inner: Mutex::new(Inner {
                next_id: 1,
                pending: None,
                session,
                attempted: false,
                phase: "idle".into(),
            }),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn request(
        &self,
        inner: &mut Inner,
        stage: Stage,
        origin: String,
        label: String,
        body: Vec<u8>,
        authorization: Option<String>,
    ) -> EnrollmentRequest {
        let path = match stage {
            Stage::Descriptor => "/remote/v1/descriptor",
            Stage::Redeem => PAIR_PATH,
            Stage::Authenticate => SESSION_CHECK_PATH,
            Stage::Saving => unreachable!("storage has no transport request"),
        };
        let id = inner.next_id;
        inner.next_id += 1;
        inner.phase = match stage {
            Stage::Descriptor => "verifying",
            Stage::Redeem => "redeeming",
            Stage::Saving => "saving",
            Stage::Authenticate => "authenticating",
        }
        .into();
        inner.pending = Some(Pending {
            id,
            stage,
            origin: origin.clone(),
            label,
        });
        EnrollmentRequest {
            request_id: id,
            url: format!("{origin}{path}"),
            host: url::Url::parse(&origin)
                .expect("validated origin")
                .host_str()
                .expect("validated host")
                .trim_start_matches('[')
                .trim_end_matches(']')
                .into(),
            spki_sha256: self.host.spki_sha256.clone(),
            method: if body.is_empty() { "GET" } else { "POST" }.into(),
            body,
            authorization,
            max_body_bytes: 16 * 1024,
        }
    }
    fn authenticate(&self, inner: &mut Inner, origin: String, label: String) -> EnrollmentAction {
        let Some(mac) = &inner.session else {
            return rejected(inner, "Session is unavailable; pair again");
        };
        let authorization = Some(format!("Bearer {}", mac.session_token));
        let body =
            serde_json::to_vec(&json!({"target": self.host.target})).expect("target serializes");
        let request = self.request(
            inner,
            Stage::Authenticate,
            origin,
            label,
            body,
            authorization,
        );
        EnrollmentAction::Request { request }
    }
    fn session_reply(&self, body: &[u8], label: &str) -> Option<SavedMac> {
        let value: Value = serde_json::from_slice(body).ok()?;
        let target = serde_json::from_value(value["target"].clone()).ok()?;
        if self.host.target != target || value["revoked"] != false {
            return None;
        }
        let scopes: Vec<String> = serde_json::from_value(value["scopes"].clone()).ok()?;
        if self.ticket.as_ref()?.scopes != scopes {
            return None;
        }
        let expiry = value["expiresAtMs"].as_u64()?;
        let token = value["sessionToken"].as_str()?;
        let id = value["id"].as_str()?;
        if !secret(token)
            || id.is_empty()
            || id.len() > 128
            || expiry <= now()
            || expiry > now().saturating_add(30 * 86_400_000)
        {
            return None;
        }
        Some(SavedMac {
            host_name: self
                .ticket
                .as_ref()?
                .host_name
                .clone()
                .unwrap_or_else(|| "Paired Mac".into()),
            host_ticket: serde_json::to_string(&self.host).ok()?,
            session_id: id.into(),
            session_token: token.into(),
            expires_at_ms: expiry,
            scopes,
            client_label: label.into(),
        })
    }
}

/// The one validator for saved access: restore (RM-02) and the SDK session
/// (IOS-00) accept exactly the same records.
pub(crate) fn validate_saved(mac: &SavedMac) -> Result<HostTicket, EnrollmentError> {
    let host: HostTicket =
        serde_json::from_str(&mac.host_ticket).map_err(|_| EnrollmentError::InvalidHost)?;
    validate_host(&host)?;
    if mac.session_id.is_empty()
        || mac.session_id.len() > 128
        || !secret(&mac.session_token)
        || !scopes_valid(&mac.scopes)
    {
        return Err(EnrollmentError::InvalidSession);
    }
    Ok(host)
}

fn validate_host(host: &HostTicket) -> Result<(), EnrollmentError> {
    let text = serde_json::to_string(host).map_err(|_| EnrollmentError::InvalidHost)?;
    parse_ticket(&text).map_err(|_| EnrollmentError::InvalidHost)?;
    if host.target.device_id.is_empty()
        || host.target.environment_id.is_empty()
        || host.target.device_id.len() > 128
        || host.target.environment_id.len() > 128
        || host.target.protocol_version != 1
        || host.target.required_capabilities != ["remote-auth-v1"]
    {
        return Err(EnrollmentError::InvalidHost);
    }
    for endpoint in &host.endpoints {
        let parsed = url::Url::parse(endpoint).map_err(|_| EnrollmentError::InvalidHost)?;
        let name = parsed
            .host_str()
            .ok_or(EnrollmentError::InvalidHost)?
            .trim_matches(['[', ']']);
        if name.eq_ignore_ascii_case("localhost")
            || name
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
        {
            return Err(EnrollmentError::InvalidHost);
        }
    }
    Ok(())
}
fn secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn scopes_valid(scopes: &[String]) -> bool {
    !scopes.is_empty()
        && scopes.len() <= 5
        && scopes.iter().enumerate().all(|(i, s)| {
            [
                "environment-read",
                "tasks-read",
                "files-read",
                "terminal-operate",
                // RM-04: task commands through ExecuteRemoteTaskCommand.
                "tasks-operate",
            ]
            .contains(&s.as_str())
                && !scopes[..i].contains(s)
        })
}
fn rejected(inner: &mut Inner, reason: &str) -> EnrollmentAction {
    inner.phase = "rejected".into();
    EnrollmentAction::Rejected {
        reason: reason.into(),
    }
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
