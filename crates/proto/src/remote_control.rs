//! Direct remote control wire contract (Control this Mac, RM-00/M0).
//!
//! A host is identified by its [`RemoteTarget`] and the SHA-256 of its TLS
//! public key (SubjectPublicKeyInfo, DER). A route (address, hostname, port)
//! is never identity: the same host may be reached at several endpoints, and
//! a phone trusts a connection only when the presented key matches the pin it
//! obtained out of band, and the descriptor read over that connection names
//! the expected target.

use serde::{Deserialize, Serialize};

use crate::WorkspaceScope;

/// Direct transport protocol negotiated separately from `remote-auth-v1`.
pub const REMOTE_DIRECT_V1: &str = "remote-direct-v1";

/// Path of the unauthenticated, harmless host descriptor.
pub const DESCRIPTOR_PATH: &str = "/remote/v1/descriptor";

/// Path of the minimal unauthenticated availability check.
pub const HEALTH_PATH: &str = "/remote/v1/health";

/// Redeem a one-time pairing credential (body: `RedeemPairing`).
pub const PAIR_PATH: &str = "/remote/v1/pair";

/// Check a device session (bearer token; body: `{target}`).
pub const SESSION_CHECK_PATH: &str = "/remote/v1/session/check";

/// Issue a single-use socket ticket (bearer token; body: `{target}`).
pub const SOCKET_TICKET_PATH: &str = "/remote/v1/socket-ticket";

/// The RPC WebSocket (`Authorization: Ticket <ticket>`; no `Origin`).
pub const SOCKET_PATH: &str = "/remote/v1/ws";

/// Current [`HostTicket`] format.
pub const HOST_TICKET_VERSION: u16 = 1;

/// The environment a remote session acts on. Pairing, authentication and the
/// direct descriptor all carry this exact value; any field differing means a
/// different host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteTarget {
    pub device_id: String,
    pub workspace_scope: WorkspaceScope,
    pub environment_id: String,
    pub protocol_version: u16,
    pub required_capabilities: Vec<String>,
}

/// What `GET /remote/v1/descriptor` returns. It is public: target identity,
/// protocol, catalog version and the public-key pin, nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDescriptor {
    pub protocol: String,
    pub target: RemoteTarget,
    /// The operation catalog version (CORE-01) the host serves.
    pub catalog_version: u16,
    /// Lowercase hex SHA-256 of the host's TLS SubjectPublicKeyInfo (DER).
    pub spki_sha256: String,
}

/// The trust half of the future pairing QR: which host to expect, where it
/// may be reached and which key it must present. It carries no credential;
/// M1 adds the one-time pairing secret alongside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostTicket {
    pub version: u16,
    pub target: RemoteTarget,
    /// `https://host:port` candidates, best first.
    pub endpoints: Vec<String>,
    pub spki_sha256: String,
}

/// Explicit native enrollment input. The secret travels only in the local
/// QR/manual fragment and the pinned redemption body, never an HTTP URL.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingTicket {
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    pub host: HostTicket,
    pub pairing_id: String,
    pub credential: String,
    pub expires_at_ms: u64,
    pub scopes: Vec<String>,
}

impl PairingTicket {
    pub const PREFIX: &str = "luna://pair#";
    pub const MAX_BYTES: usize = 8192;

    pub fn encode(&self) -> Result<String, &'static str> {
        use base64::Engine;
        let bytes = serde_json::to_vec(self).map_err(|_| "Cannot encode pairing")?;
        let code = format!(
            "{}{}",
            Self::PREFIX,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        );
        (code.len() <= Self::MAX_BYTES)
            .then_some(code)
            .ok_or("Pairing code is too large")
    }

    pub fn decode(code: &str) -> Result<Self, &'static str> {
        use base64::Engine;
        if code.len() > Self::MAX_BYTES {
            return Err("Pairing code is too large");
        }
        let encoded = code
            .trim()
            .strip_prefix(Self::PREFIX)
            .ok_or("Not a LUNA pairing code")?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "Invalid pairing code")?;
        serde_json::from_slice(&bytes).map_err(|_| "Invalid pairing code")
    }
}

/// Whether `value` is a well-formed pin: 64 lowercase hex digits.
pub fn is_spki_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
