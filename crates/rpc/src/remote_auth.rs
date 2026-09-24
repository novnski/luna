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
use luna_proto::WorkspaceScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::{RpcClient, RpcError, RpcReply, RpcService, methods, parse_params};

pub const PROTOCOL_VERSION: u16 = 1;
pub const CAPABILITY: &str = luna_proto::capabilities::REMOTE_AUTH_V1;
const MAX_PAIRING_TTL_SECS: u64 = 600;
const MAX_SESSION_TTL_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteScope {
    EnvironmentRead,
    TasksRead,
    FilesRead,
    TerminalOperate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEnvironment {
    pub device_id: String,
    pub workspace_scope: WorkspaceScope,
    pub environment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteTarget {
    pub device_id: String,
    pub workspace_scope: WorkspaceScope,
    pub environment_id: String,
    pub protocol_version: u16,
    pub required_capabilities: Vec<String>,
}

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
    pub client_label: String,
    pub scopes: Vec<RemoteScope>,
    pub expires_at_ms: u64,
    pub revoked: bool,
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
            serde_json::from_str::<State>(&text)
                .map_err(|_| denied("invalid remote authority state"))?
        } else {
            // A dangling symlink must not be treated as an absent store.
            if fs::symlink_metadata(&path).is_ok() {
                return Err(denied("remote authority path is not a regular file"));
            }
            State {
                version: PROTOCOL_VERSION,
                environment: environment.clone(),
                pairings: Vec::new(),
                sessions: Vec::new(),
            }
        };
        if state.version != PROTOCOL_VERSION || state.environment != environment {
            return Err(denied(
                "remote authority belongs to a different environment or protocol",
            ));
        }
        let (changed, _) = watch::channel(0);
        Ok(Arc::new(Self {
            path,
            state: Mutex::new(state),
            changed,
        }))
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
                client_label: request.client_label,
                scopes: pairing.view.scopes,
                expires_at_ms: now.saturating_add(pairing.session_ttl_secs * 1000),
                revoked: false,
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

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

/// Per-connection adapter used by the real device-room virtual connection.
/// A future direct remote transport must use this same adapter. Local IPC never
/// constructs it and retains its existing process-local trust boundary.
pub struct RemoteRpcService {
    inner: Arc<dyn RpcService>,
    auth: Arc<RemoteAuthStore>,
    token: Mutex<Option<String>>,
}

impl RemoteRpcService {
    pub fn new(inner: Arc<dyn RpcService>, auth: Arc<RemoteAuthStore>) -> Self {
        Self {
            inner,
            auth,
            token: Mutex::new(None),
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
            *lock(&self.token) = None;
            let request: AuthenticateRemote = parse_params(params)?;
            self.auth.check_target(&request.target)?;
            let session = self.auth.validate_token(&request.session_token)?;
            *lock(&self.token) = Some(request.session_token);
            return RpcReply::value(&session);
        }
        let policy =
            method_policy(method).ok_or_else(|| RpcError::UnknownMethod(method.to_owned()))?;
        let Some(required_scope) = policy else {
            return Err(denied("method is local only"));
        };
        let token = lock(&self.token)
            .clone()
            .ok_or_else(|| denied("device session required"))?;
        let session = self.auth.validate_token(&token)?;
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
        let reply = self.inner.handle(method, params).await?;
        match reply {
            RpcReply::Value(value) => {
                self.auth.validate_token(&token)?;
                Ok(RpcReply::Value(value))
            }
            RpcReply::Stream(source) => {
                let auth = self.auth.clone();
                let changes = auth.subscribe();
                let stream = stream::unfold(
                    (source, changes, auth, token),
                    |(mut source, mut changes, auth, token)| async move {
                        loop {
                            let session = auth.validate_token(&token).ok()?;
                            let wait = Duration::from_millis(
                                session.expires_at_ms.saturating_sub(now_ms()),
                            );
                            tokio::select! {
                                item = source.next() => {
                                    let item = item?;
                                    auth.validate_token(&token).ok()?;
                                    return Some((item, (source, changes, auth, token)));
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

/// Known shipped methods are explicitly inventoried in `methods::ALL`; an
/// unlisted/new method fails closed even if the engine later implements it.
fn method_policy(method: &str) -> Option<Option<RemoteScope>> {
    let scope = match method {
        methods::LOCAL_OWNER_ROOT_CLAIM => None,
        methods::ENGINE_INFO
        | methods::ENGINE_READY
        | methods::LIST_HARNESSES
        | methods::LIST_MODELS
        | methods::LIST_COMMANDS => Some(RemoteScope::EnvironmentRead),
        methods::GET_TASK_IDENTITY
        | methods::GET_RUNTIME_CONTRACT
        | methods::GET_TASK_COMMAND_RECEIPT
        | methods::GET_TASK_COMMAND_SNAPSHOT
        | methods::WATCH_TASK_COMMANDS
        | methods::WATCH_DOC_MESSAGES
        | methods::WATCH_QUEUE => Some(RemoteScope::TasksRead),
        methods::LIST_REPOS
        | methods::LIST_BRANCHES
        | methods::LIST_REFS
        | methods::LIST_GIT_HISTORY
        | methods::SEARCH_GIT_HISTORY
        | methods::LIST_WORKSPACE_DIRECTORY
        | methods::SEARCH_WORKSPACE_FILES
        | methods::READ_WORKSPACE_FILE
        | methods::READ_WORKSPACE_IMAGE
        | methods::WATCH_WORKSPACE_FILES
        | methods::GET_CHECKOUT_DIFF
        | methods::GET_CHECKOUT_FILE_DIFF_TEXT
        | methods::WATCH_CHECKOUT_DIFFS
        | methods::WATCH_CHECKOUT_CHANGE_REQUEST
        | methods::LIST_TERMINALS => Some(RemoteScope::FilesRead),
        methods::OPEN_TERMINAL
        | methods::SUBSCRIBE_TERMINAL
        | methods::WRITE_TERMINAL
        | methods::RESIZE_TERMINAL
        | methods::CLOSE_TERMINAL => Some(RemoteScope::TerminalOperate),
        _ if methods::ALL.contains(&method) => None,
        _ => return None,
    };
    Some(scope)
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
