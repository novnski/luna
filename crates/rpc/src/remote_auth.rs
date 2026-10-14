//! L29 remote authority for the existing RPC dispatch. The local IPC owner
//! creates grants; a remote connection redeems one grant, authenticates its
//! device session, then receives a method-specific scope check for every call.
//! No transport address, relay token, or loopback route is an RPC principal.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use getrandom::fill;
use hmac::{Hmac, Mac};
use luna_proto::WorkspaceScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::{RpcClient, RpcError, RpcReply, RpcService, methods, parse_params};

pub const PROTOCOL_VERSION: u16 = 1;
pub const CAPABILITY: &str = luna_proto::capabilities::REMOTE_AUTH_V1;
/// On-disk store version. v2 records each session's last use; a v1 store
/// migrates on open. A newer store is refused, never guessed at.
const STORE_VERSION: u16 = 2;
/// A socket ticket's lifetime. Tickets are single use and kept only in
/// memory; per the owner decision they stay valid until expiry even after
/// their session is revoked, and the socket still re-checks every call.
pub const SOCKET_TICKET_TTL_MS: u64 = 30_000;
/// A session's recorded last use is refreshed at most this often.
const LAST_USE_GRANULARITY_MS: u64 = 60_000;
const MAX_PAIRING_TTL_SECS: u64 = 600;
const MAX_SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RemoteScope {
    EnvironmentRead,
    TasksRead,
    FilesRead,
    TerminalOperate,
    // RM-04: create, send to, queue, steer, stop and archive this host's
    // tasks through `ExecuteRemoteTaskCommand`. Every call of a method with
    // this scope carries the typed `RemotePrincipal` the guard validated.
    // (A plain comment: a doc comment would change the published schema.)
    TasksOperate,
}

/// RM-04: who a remote command comes from. Only [`RemoteRpcService`] builds
/// it, from the device session it validated for that very call, and hands it
/// to [`crate::RpcService::handle_remote`] as a Rust value: no caller can put
/// one in parameters, and the local owner's IPC never carries one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemotePrincipal {
    pub session_id: String,
    pub pairing_id: Option<String>,
    pub client_label: String,
    pub scopes: Vec<RemoteScope>,
    pub expires_at_ms: u64,
    /// The host environment the session belongs to.
    pub environment: RemoteEnvironment,
}

impl RemotePrincipal {
    fn of(session: &SessionView, environment: RemoteEnvironment) -> Self {
        Self {
            session_id: session.id.clone(),
            pairing_id: session.pairing_id.clone(),
            client_label: session.client_label.clone(),
            scopes: session.scopes.clone(),
            expires_at_ms: session.expires_at_ms,
            environment,
        }
    }

    /// The receipt actor of this session's commands, stable for its life.
    pub fn actor_id(&self) -> String {
        luna_proto::remote_command::remote_actor_id(&self.session_id)
    }
}

/// RM-04: a remote command's payload identity, recorded as its receipt's
/// `payloadHash`: SHA-256 of the canonical (sorted-key) JSON of its task id
/// and intent. Host and phone compute it the same way whatever their
/// `serde_json` map ordering, so a phone can tell its own receipt from
/// another payload's under the same command id. Fences, issue time and the
/// command id itself are not part of it.
pub fn remote_command_identity(command: &luna_proto::remote_command::RemoteTaskCommand) -> String {
    let value = serde_json::json!({
        "v": 1,
        "taskId": command.task_id,
        "intent": command.intent,
    });
    let mut encoded = String::new();
    canonical_json(&value, &mut encoded);
    format!("remote-v1:{}", hex(&Sha256::digest(encoded.as_bytes())))
}

fn canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                canonical_json(&map[key.as_str()], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(value, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEnvironment {
    pub device_id: String,
    pub workspace_scope: WorkspaceScope,
    pub environment_id: String,
}

pub use luna_proto::remote_control::RemoteTarget;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePairing {
    pub label: String,
    pub scopes: Vec<RemoteScope>,
    pub pairing_ttl_secs: u64,
    pub session_ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    pub id: String,
    pub label: String,
    pub scopes: Vec<RemoteScope>,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedPairing {
    #[serde(flatten)]
    pub pairing: PairingView,
    pub credential: String,
    pub target: RemoteTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedeemPairing {
    pub target: RemoteTarget,
    pub credential: String,
    pub client_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticateRemote {
    pub target: RemoteTarget,
    pub session_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    /// Host-created bootstrap that issued this grant. Optional for old stores;
    /// labels are never used to correlate authority with the enrollment UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_id: Option<String>,
    pub client_label: String,
    pub scopes: Vec<RemoteScope>,
    pub expires_at_ms: u64,
    pub revoked: bool,
    /// When the session last opened a connection (store v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<u64>,
}

/// A single-use credential that opens one WebSocket for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SocketTicket {
    pub ticket: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedSession {
    #[serde(flatten)]
    pub session: SessionView,
    pub session_token: String,
    pub target: RemoteTarget,
}

impl RpcClient {
    /// Redeem a host-issued one-time pairing credential over a guarded remote
    /// connection. The returned token is shown only to this caller.
    pub async fn redeem_remote_pairing(
        &self,
        request: RedeemPairing,
    ) -> Result<IssuedSession, RpcError> {
        let params = serde_json::to_value(request)
            .map_err(|error| RpcError::BadParams(error.to_string()))?;
        self.call_as(methods::REDEEM_REMOTE_PAIRING, params).await
    }

    /// Authenticate this connection before calling scoped methods. A new
    /// connection, including after reconnect, must authenticate again.
    pub async fn authenticate_remote(
        &self,
        request: AuthenticateRemote,
    ) -> Result<SessionView, RpcError> {
        let params = serde_json::to_value(request)
            .map_err(|error| RpcError::BadParams(error.to_string()))?;
        self.call_as(methods::AUTHENTICATE_REMOTE, params).await
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredPairing {
    view: PairingView,
    credential_hash: String,
    session_ttl_secs: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredSession {
    view: SessionView,
    token_hash: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u16,
    environment: RemoteEnvironment,
    pairings: Vec<StoredPairing>,
    sessions: Vec<StoredSession>,
}

/// One store per engine profile. The file contains only SHA-256 hashes of
/// random 256-bit credentials; it is created private and published atomically.
pub struct RemoteAuthStore {
    path: PathBuf,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    /// Nonce hash → (session id, expiry). Never persisted.
    tickets: Mutex<std::collections::HashMap<String, (String, u64)>>,
    /// One profile authority owner's epoch key. Like the ticket inventory,
    /// it is never persisted: outstanding tickets do not survive restart.
    ticket_signing_key: [u8; 32],
}

impl RemoteAuthStore {
    pub fn open(root: &Path, environment: RemoteEnvironment) -> Result<Arc<Self>, RpcError> {
        let path = root.join("remote-auth.json");
        let state = if path.exists() {
            private_file(&path)?;
            let mut text = String::new();
            File::open(&path)
                .and_then(|mut file| file.read_to_string(&mut text))
                .map_err(storage_error)?;
            let mut state = serde_json::from_str::<State>(&text)
                .map_err(|_| denied("invalid remote authority state"))?;
            if state.version == 1 && state.environment == environment {
                // v1 → v2 adds only optional fields: publish it atomically.
                // A failed publish leaves the v1 file as it was.
                state.version = STORE_VERSION;
                if !persist(&path, &state)? {
                    tracing::warn!("remote authority migrated; directory sync uncertain");
                }
            }
            state
        } else {
            // A dangling symlink must not be treated as an absent store.
            if fs::symlink_metadata(&path).is_ok() {
                return Err(denied("remote authority path is not a regular file"));
            }
            State {
                version: STORE_VERSION,
                environment: environment.clone(),
                pairings: Vec::new(),
                sessions: Vec::new(),
            }
        };
        if state.version != STORE_VERSION || state.environment != environment {
            return Err(denied(
                "remote authority belongs to a different environment or store version",
            ));
        }
        let (changed, _) = watch::channel(0);
        let mut ticket_signing_key = [0; 32];
        fill(&mut ticket_signing_key).map_err(|_| denied("secure random source unavailable"))?;
        Ok(Arc::new(Self {
            path,
            state: Mutex::new(state),
            changed,
            tickets: Mutex::default(),
            ticket_signing_key,
        }))
    }

    /// Explicit recovery for a store that cannot be opened (corrupt, foreign
    /// or unreadable): move it aside, keeping its bytes, so the next start
    /// begins empty and devices pair again. A healthy store is never reset.
    pub fn quarantine_unreadable(
        root: &Path,
        environment: RemoteEnvironment,
    ) -> Result<Option<PathBuf>, RpcError> {
        let path = root.join("remote-auth.json");
        if fs::symlink_metadata(&path).is_err() {
            return Ok(None);
        }
        if Self::open(root, environment).is_ok() {
            return Err(denied("remote authority is healthy; nothing to reset"));
        }
        let quarantined = root.join(format!("remote-auth.corrupt-{}.json", now_ms()));
        fs::rename(&path, &quarantined).map_err(storage_error)?;
        Ok(Some(quarantined))
    }

    pub fn environment(&self) -> RemoteEnvironment {
        lock(&self.state).environment.clone()
    }

    pub fn target(&self) -> RemoteTarget {
        let environment = self.environment();
        RemoteTarget {
            device_id: environment.device_id,
            workspace_scope: environment.workspace_scope,
            environment_id: environment.environment_id,
            protocol_version: PROTOCOL_VERSION,
            required_capabilities: vec![CAPABILITY.to_owned()],
        }
    }

    fn check_target(&self, target: &RemoteTarget) -> Result<(), RpcError> {
        let expected = self.environment();
        if target.device_id != expected.device_id
            || target.workspace_scope != expected.workspace_scope
            || target.environment_id != expected.environment_id
        {
            return Err(denied("target environment mismatch"));
        }
        if target.protocol_version != PROTOCOL_VERSION
            || !target
                .required_capabilities
                .iter()
                .any(|capability| capability == CAPABILITY)
            || target.required_capabilities.iter().any(|capability| {
                capability != CAPABILITY
                    && !luna_proto::capabilities::CURRENT.contains(&capability.as_str())
            })
        {
            return Err(denied("remote protocol or capability mismatch"));
        }
        Ok(())
    }

    pub fn create_pairing(&self, request: CreatePairing) -> Result<IssuedPairing, RpcError> {
        if request.label.trim().is_empty() || request.label.len() > 100 {
            return Err(denied("pairing label must be 1-100 bytes"));
        }
        if request.scopes.is_empty() || has_duplicate_scopes(&request.scopes) {
            return Err(denied("pairing scopes must be nonempty and unique"));
        }
        if !(1..=MAX_PAIRING_TTL_SECS).contains(&request.pairing_ttl_secs)
            || !(1..=MAX_SESSION_TTL_SECS).contains(&request.session_ttl_secs)
        {
            return Err(denied("pairing or session lifetime is out of range"));
        }
        let credential = random_secret()?;
        let view = PairingView {
            id: uuid::Uuid::new_v4().to_string(),
            label: request.label,
            scopes: request.scopes,
            expires_at_ms: now_ms().saturating_add(request.pairing_ttl_secs * 1000),
        };
        self.mutate(|state| {
            state
                .pairings
                .retain(|pairing| pairing.view.expires_at_ms > now_ms());
            state.pairings.push(StoredPairing {
                view: view.clone(),
                credential_hash: hash_secret(&credential),
                session_ttl_secs: request.session_ttl_secs,
            });
            Ok(())
        })?;
        Ok(IssuedPairing {
            pairing: view,
            credential,
            target: self.target(),
        })
    }

    pub fn list_pairings(&self) -> Vec<PairingView> {
        let now = now_ms();
        lock(&self.state)
            .pairings
            .iter()
            .filter(|pairing| pairing.view.expires_at_ms > now)
            .map(|pairing| pairing.view.clone())
            .collect()
    }

    pub fn revoke_pairing(&self, id: &str) -> Result<bool, RpcError> {
        self.mutate(|state| {
            let before = state.pairings.len();
            state.pairings.retain(|pairing| pairing.view.id != id);
            Ok(before != state.pairings.len())
        })
    }

    /// Host disable discards pending bootstrap authority but keeps sessions.
    pub fn revoke_all_pairings(&self) -> Result<(), RpcError> {
        self.mutate(|state| {
            state.pairings.clear();
            Ok(())
        })
    }

    pub fn redeem(&self, request: RedeemPairing) -> Result<IssuedSession, RpcError> {
        self.check_target(&request.target)?;
        if request.client_label.trim().is_empty() || request.client_label.len() > 100 {
            return Err(denied("client label must be 1-100 bytes"));
        }
        let token = random_secret()?;
        let credential_hash = hash_secret(&request.credential);
        let view = self.mutate(|state| {
            let now = now_ms();
            let index = state
                .pairings
                .iter()
                .position(|pairing| {
                    pairing.view.expires_at_ms > now
                        && constant_time_eq(
                            pairing.credential_hash.as_bytes(),
                            credential_hash.as_bytes(),
                        )
                })
                .ok_or_else(|| denied("pairing unavailable, expired, or already used"))?;
            let pairing = state.pairings.remove(index);
            let view = SessionView {
                id: uuid::Uuid::new_v4().to_string(),
                pairing_id: Some(pairing.view.id),
                client_label: request.client_label,
                scopes: pairing.view.scopes,
                expires_at_ms: now.saturating_add(pairing.session_ttl_secs * 1000),
                revoked: false,
                last_used_at_ms: None,
            };
            state.sessions.push(StoredSession {
                view: view.clone(),
                token_hash: hash_secret(&token),
            });
            Ok(view)
        })?;
        Ok(IssuedSession {
            session: view,
            session_token: token,
            target: self.target(),
        })
    }

    pub fn list_sessions(&self) -> Vec<SessionView> {
        lock(&self.state)
            .sessions
            .iter()
            .map(|session| session.view.clone())
            .collect()
    }

    pub fn revoke_session(&self, id: &str) -> Result<bool, RpcError> {
        self.mutate(|state| {
            let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| session.view.id == id)
            else {
                return Ok(false);
            };
            let changed = !session.view.revoked;
            session.view.revoked = true;
            Ok(changed)
        })
    }

    /// A device session presented with its target, as an HTTP route sees it.
    pub fn authenticate(
        &self,
        target: &RemoteTarget,
        token: &str,
    ) -> Result<SessionView, RpcError> {
        self.check_target(target)?;
        self.validate_token(token)
    }

    /// A live session by id: the check a ticket-opened socket repeats.
    pub fn validate_session(&self, id: &str) -> Result<SessionView, RpcError> {
        let state = lock(&self.state);
        let session = state
            .sessions
            .iter()
            .find(|session| session.view.id == id)
            .ok_or_else(|| denied("unknown device session"))?;
        if session.view.revoked || session.view.expires_at_ms <= now_ms() {
            return Err(denied("device session revoked or expired"));
        }
        Ok(session.view.clone())
    }

    pub fn issue_socket_ticket(
        &self,
        target: &RemoteTarget,
        token: &str,
    ) -> Result<SocketTicket, RpcError> {
        let session = self.authenticate(target, token)?;
        let nonce = random_secret()?;
        let now = now_ms();
        let expires_at_ms = now.saturating_add(SOCKET_TICKET_TTL_MS);
        let mac = self.socket_ticket_mac(&self.target(), &nonce, &session.id, expires_at_ms);
        let ticket = format!("st1.{nonce}.{}", hex(&mac.finalize().into_bytes()));
        let mut tickets = lock(&self.tickets);
        tickets.retain(|_, (_, expires)| *expires > now);
        tickets.insert(hash_secret(&nonce), (session.id, expires_at_ms));
        Ok(SocketTicket {
            ticket,
            expires_at_ms,
        })
    }

    /// Consumes a ticket and names its session. Revocation is not checked
    /// here (owner decision); the socket checks the session on every call.
    pub fn redeem_socket_ticket(&self, ticket: &str) -> Result<String, RpcError> {
        let (nonce, signature) = ticket
            .strip_prefix("st1.")
            .and_then(|ticket| ticket.split_once('.'))
            .filter(|(nonce, signature)| {
                nonce.len() == 64
                    && signature.len() == 64
                    && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && signature.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or_else(|| denied("invalid socket ticket format"))?;
        let mut tag = [0; 32];
        for (index, byte) in tag.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&signature[index * 2..index * 2 + 2], 16)
                .map_err(|_| denied("invalid socket ticket signature"))?;
        }
        let target = self.target();
        let nonce_hash = hash_secret(nonce);
        let mut tickets = lock(&self.tickets);
        let (session, expires_at_ms) = tickets
            .get(&nonce_hash)
            .cloned()
            .ok_or_else(|| denied("socket ticket unknown or already used"))?;
        self.socket_ticket_mac(&target, nonce, &session, expires_at_ms)
            .verify_slice(&tag)
            .map_err(|_| denied("invalid socket ticket signature"))?;
        // Verify before consuming, under the same lock as the one-use check:
        // a changed signature neither burns the original nor permits replay.
        tickets.remove(&nonce_hash);
        if expires_at_ms <= now_ms() {
            return Err(denied("socket ticket expired"));
        }
        Ok(session)
    }

    fn socket_ticket_mac(
        &self,
        target: &RemoteTarget,
        nonce: &str,
        session: &str,
        expires_at_ms: u64,
    ) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.ticket_signing_key)
            .expect("HMAC accepts the fixed 256-bit key");
        mac.update(b"luna-remote-socket-ticket-v1\0");
        mac.update(
            &serde_json::to_vec(&(target, session, expires_at_ms, nonce))
                .expect("the typed ticket claims serialize"),
        );
        mac
    }

    /// Records that a session connected, at most once a minute.
    pub fn record_use(&self, id: &str) -> Result<SessionView, RpcError> {
        let view = self.validate_session(id)?;
        let now = now_ms();
        if view
            .last_used_at_ms
            .is_some_and(|last| now.saturating_sub(last) < LAST_USE_GRANULARITY_MS)
        {
            return Ok(view);
        }
        self.mutate(|state| {
            let session = state
                .sessions
                .iter_mut()
                .find(|session| session.view.id == id)
                .ok_or_else(|| denied("unknown device session"))?;
            session.view.last_used_at_ms = Some(now);
            Ok(session.view.clone())
        })
    }

    fn validate_token(&self, token: &str) -> Result<SessionView, RpcError> {
        let hash = hash_secret(token);
        let state = lock(&self.state);
        let session = state
            .sessions
            .iter()
            .find(|session| constant_time_eq(session.token_hash.as_bytes(), hash.as_bytes()))
            .ok_or_else(|| denied("unknown device session"))?;
        if session.view.revoked || session.view.expires_at_ms <= now_ms() {
            return Err(denied("device session revoked or expired"));
        }
        Ok(session.view.clone())
    }

    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut State) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let mut state = lock(&self.state);
        let mut next = state.clone();
        let result = change(&mut next)?;
        let published = persist(&self.path, &next)?;
        *state = next;
        self.changed.send_modify(|version| *version += 1);
        if !published {
            return Err(denied(
                "remote authority was published but durability is uncertain; inspect state before retrying",
            ));
        }
        Ok(result)
    }

    /// Wakes on every change, including revocations.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

/// Per-connection adapter used by the real device-room virtual connection.
/// A future direct remote transport must use this same adapter. Local IPC never
/// constructs it and retains its existing process-local trust boundary.
pub struct RemoteRpcService {
    inner: Arc<dyn RpcService>,
    auth: Arc<RemoteAuthStore>,
    credential: Mutex<Option<Credential>>,
}

/// What a connection authenticated with; checked again on every call.
#[derive(Clone)]
enum Credential {
    Token(String),
    /// A socket opened with a ticket for this session.
    Session(String),
}

impl Credential {
    fn validate(&self, auth: &RemoteAuthStore) -> Result<SessionView, RpcError> {
        match self {
            Self::Token(token) => auth.validate_token(token),
            Self::Session(id) => auth.validate_session(id),
        }
    }
}

impl RemoteRpcService {
    pub fn new(inner: Arc<dyn RpcService>, auth: Arc<RemoteAuthStore>) -> Self {
        Self {
            inner,
            auth,
            credential: Mutex::new(None),
        }
    }

    /// A socket opened with a ticket: already bound to `session_id`.
    pub fn for_session(
        inner: Arc<dyn RpcService>,
        auth: Arc<RemoteAuthStore>,
        session_id: String,
    ) -> Self {
        Self {
            inner,
            auth,
            credential: Mutex::new(Some(Credential::Session(session_id))),
        }
    }
}

#[async_trait]
impl RpcService for RemoteRpcService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == methods::REDEEM_REMOTE_PAIRING {
            let request: RedeemPairing = parse_params(params)?;
            return RpcReply::value(&self.auth.redeem(request)?);
        }
        if method == methods::AUTHENTICATE_REMOTE {
            *lock(&self.credential) = None;
            let request: AuthenticateRemote = parse_params(params)?;
            let session = self
                .auth
                .authenticate(&request.target, &request.session_token)?;
            *lock(&self.credential) = Some(Credential::Token(request.session_token));
            return RpcReply::value(&session);
        }
        let policy =
            method_policy(method).ok_or_else(|| RpcError::UnknownMethod(method.to_owned()))?;
        let Some(required_scope) = policy else {
            return Err(denied("method is local only"));
        };
        let credential = lock(&self.credential)
            .clone()
            .ok_or_else(|| denied("device session required"))?;
        let session = credential.validate(&self.auth)?;
        if !session.scopes.contains(&required_scope) {
            return Err(denied("device session lacks required scope"));
        }
        // Every accepted call is for the engine that owns this store. Do not
        // turn an authenticated host into a generic proxy to a third machine.
        if params
            .get("targetDeviceId")
            .is_some_and(|value| value.as_str() != Some(&self.auth.environment().device_id))
        {
            return Err(denied("target device mismatch"));
        }
        if required_scope == RemoteScope::TasksOperate {
            // RM-04: a principal-bound command. The engine admits it with
            // this session as its receipt's actor and re-checks the grant;
            // nothing in the parameters can name another principal. Its
            // reply is the durable receipt of an admission made under a
            // valid grant, so a later revocation does not turn it into an
            // error (the next call is refused instead).
            let principal = RemotePrincipal::of(&session, self.auth.environment());
            return self
                .inner
                .clone()
                .handle_remote(principal, method.to_owned(), params)
                .await;
        }
        let reply = self.inner.handle(method, params).await?;
        match reply {
            RpcReply::Value(value) => {
                credential.validate(&self.auth)?;
                Ok(RpcReply::Value(value))
            }
            RpcReply::Stream(source) => {
                let auth = self.auth.clone();
                let changes = auth.subscribe();
                let stream = stream::unfold(
                    (source, changes, auth, credential),
                    |(mut source, mut changes, auth, credential)| async move {
                        loop {
                            let session = credential.validate(&auth).ok()?;
                            let wait = Duration::from_millis(
                                session.expires_at_ms.saturating_sub(now_ms()),
                            );
                            tokio::select! {
                                item = source.next() => {
                                    let item = item?;
                                    credential.validate(&auth).ok()?;
                                    return Some((item, (source, changes, auth, credential)));
                                }
                                result = changes.changed() => {
                                    result.ok()?;
                                }
                                _ = tokio::time::sleep(wait) => return None,
                            }
                        }
                    },
                )
                .boxed();
                Ok(RpcReply::Stream(stream))
            }
        }
    }
}

/// Known shipped methods each have a reviewed row in the operation catalog; an
/// unlisted/new method fails closed even if the engine later implements it,
/// and a row without a remote scope stays local-only.
fn method_policy(method: &str) -> Option<Option<RemoteScope>> {
    crate::operations::spec(method).map(|op| op.remote_scope())
}

fn has_duplicate_scopes(scopes: &[RemoteScope]) -> bool {
    scopes
        .iter()
        .enumerate()
        .any(|(index, scope)| scopes[..index].contains(scope))
}

fn random_secret() -> Result<String, RpcError> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes).map_err(|_| denied("secure random source unavailable"))?;
    Ok(hex(&bytes))
}

fn hash_secret(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in a.iter().zip(b) {
        difference |= left ^ right;
    }
    difference == 0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn denied(reason: &str) -> RpcError {
    RpcError::Failed(format!("remote authorization denied: {reason}"))
}
fn storage_error(error: std::io::Error) -> RpcError {
    RpcError::Failed(format!("remote authority storage: {error}"))
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn private_file(path: &Path) -> Result<(), RpcError> {
    let metadata = fs::symlink_metadata(path).map_err(storage_error)?;
    if !metadata.file_type().is_file() {
        return Err(denied("remote authority path is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(denied("remote authority file permissions are too broad"));
        }
    }
    Ok(())
}

/// Returns false only when the rename published the new state but directory
/// fsync failed. Callers must treat that outcome as uncertain, never replay it.
fn persist(path: &Path, state: &State) -> Result<bool, RpcError> {
    let parent = path
        .parent()
        .ok_or_else(|| denied("remote authority has no parent"))?;
    fs::create_dir_all(parent).map_err(storage_error)?;
    if fs::symlink_metadata(path).is_ok() {
        private_file(path)?;
    }
    let temp = parent.join(format!(".remote-auth-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<bool, RpcError> {
        let mut file = options.open(&temp).map_err(storage_error)?;
        serde_json::to_writer(&mut file, state)
            .map_err(|error| denied(&format!("cannot encode authority state: {error}")))?;
        file.write_all(b"\n").map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
        fs::rename(&temp, path).map_err(storage_error)?;
        Ok(File::open(parent)
            .and_then(|directory| directory.sync_all())
            .is_ok())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_inventory_is_complete_and_unknown_fails_closed() {
        let source = include_str!("lib.rs");
        let declared = source
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("pub const ")?
                    .split_once(" = \"")
                    .and_then(|(_, value)| value.split('"').next())
            })
            .collect::<std::collections::HashSet<_>>();
        let inventoried = methods::ALL
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(declared, inventoried);
        for method in methods::ALL {
            assert!(method_policy(method).is_some(), "{method}");
        }
        assert_eq!(inventoried.len(), methods::ALL.len());
        assert_eq!(method_policy(methods::LOCAL_OWNER_ROOT_CLAIM), Some(None));
        assert_eq!(method_policy("FutureDangerousMethod"), None);
    }

    #[test]
    fn concurrent_pairing_redemption_persists_one_use() {
        let root = tempfile::tempdir().unwrap();
        let environment = RemoteEnvironment {
            device_id: "synthetic-device".into(),
            workspace_scope: WorkspaceScope::Local,
            environment_id: "synthetic-profile".into(),
        };
        let store = RemoteAuthStore::open(root.path(), environment.clone()).unwrap();
        let issued = store
            .create_pairing(CreatePairing {
                label: "one use".into(),
                scopes: vec![RemoteScope::EnvironmentRead],
                pairing_ttl_secs: 300,
                session_ttl_secs: 3600,
            })
            .unwrap();
        let request = RedeemPairing {
            target: issued.target,
            credential: issued.credential.clone(),
            client_label: "synthetic client".into(),
        };
        let first = store.clone();
        let second = store.clone();
        let first_request = request.clone();
        let a = std::thread::spawn(move || first.redeem(first_request));
        let b = std::thread::spawn(move || second.redeem(request));
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let reopened = RemoteAuthStore::open(root.path(), environment).unwrap();
        assert!(reopened.list_pairings().is_empty());
        assert_eq!(reopened.list_sessions().len(), 1);
        let contents = fs::read_to_string(root.path().join("remote-auth.json")).unwrap();
        assert!(!contents.contains(&issued.credential));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.path().join("remote-auth.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
