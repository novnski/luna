//! LUNA-owned credential storage.
//!
//! Provider-native authentication remains in each provider's own home. This
//! module is only for secrets LUNA owns (currently the optional sync refresh
//! token, and later integration/project credentials). The vault ciphertext is
//! stored below the selected LUNA data root; its independent random key lives
//! in the operating-system credential store.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::now_ms;

const VAULT_FORMAT: &str = "luna-environment-vault";
const VAULT_VERSION: u32 = 1;
const VAULT_CIPHER: &str = "aes-256-gcm";
const VAULT_AAD: &[u8] = b"luna-environment-vault:v1:aes-256-gcm";
const KEYCHAIN_SERVICE: &str = "com.novnski.luna.environment-vault";
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialClass {
    LunaSync,
    Integration,
    ProjectRuntime,
}

/// Exact owner boundary for one secret. Equality is the authorization check:
/// a provider/integration/project can never widen this scope at read time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialScope {
    pub class: CredentialClass,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

impl CredentialScope {
    pub fn luna_sync() -> Self {
        Self {
            class: CredentialClass::LunaSync,
            owner: "luna-sync".into(),
            project_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialState {
    Configured,
    Forgotten,
}

/// Safe for settings, diagnostics, logs, and client responses. It contains no
/// secret value, key material, nonce, ciphertext, or provider-native path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialMetadata {
    pub id: String,
    pub name: String,
    pub scope: CredentialScope,
    pub purpose: String,
    pub destination: String,
    pub state: CredentialState,
    pub revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
}

/// A scoped, revision-bound reference. Replacing or forgetting a value makes
/// an old lease unusable; callers must reacquire it from the broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialLease {
    pub id: String,
    pub revision: u64,
    pub scope: CredentialScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretIngress {
    LocalEngine,
    /// Reserved for L29-L31. L05 rejects this even when a caller claims E2EE;
    /// those steps must establish authenticated pairing and transport first.
    Remote {
        end_to_end_encrypted: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    InvalidRequest(&'static str),
    CredentialStoreUnavailable,
    CredentialStoreLocked,
    VaultKeyMissing,
    VaultCorrupt,
    NotFound,
    ScopeDenied,
    Revoked,
    RemoteSecretFlowUnavailable,
    StorageUnavailable,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidRequest(message) => message,
            Self::CredentialStoreUnavailable => {
                "the operating-system credential store is unavailable; LUNA will not store secrets as plaintext"
            }
            Self::CredentialStoreLocked => {
                "the operating-system credential store is locked; unlock it and retry"
            }
            Self::VaultKeyMissing => {
                "the encrypted LUNA vault exists but its credential-store key is missing"
            }
            Self::VaultCorrupt => "the encrypted LUNA vault could not be authenticated",
            Self::NotFound => "the requested credential is not configured",
            Self::ScopeDenied => "the requested credential is outside the caller's exact scope",
            Self::Revoked => "the credential lease is stale or has been revoked",
            Self::RemoteSecretFlowUnavailable => {
                "remote secret transfer is unavailable until LUNA pairing and end-to-end transport are implemented"
            }
            Self::StorageUnavailable => "the encrypted LUNA vault could not be updated",
        };
        f.write_str(message)
    }
}

impl std::error::Error for CredentialError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyStoreFailure {
    Unavailable,
    Locked,
}

trait VaultKeyStore: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, KeyStoreFailure>;
    fn store(&self, key: &[u8]) -> Result<(), KeyStoreFailure>;
}

#[cfg(target_os = "macos")]
struct PlatformKeyStore {
    account: String,
}

#[cfg(target_os = "macos")]
impl PlatformKeyStore {
    fn for_data_dir(data_dir: &Path) -> Self {
        let stable_path = data_dir
            .canonicalize()
            .unwrap_or_else(|_| data_dir.to_path_buf());
        let digest = Sha256::digest(stable_path.to_string_lossy().as_bytes());
        Self {
            account: format!("vault-v1-{}", hex_prefix(&digest, 16)),
        }
    }

    fn classify(error: security_framework::base::Error) -> KeyStoreFailure {
        // errSecInteractionNotAllowed, errSecAuthFailed, and userCanceled.
        match error.code() {
            -25308 | -25293 | -128 => KeyStoreFailure::Locked,
            _ => KeyStoreFailure::Unavailable,
        }
    }
}

#[cfg(target_os = "macos")]
impl VaultKeyStore for PlatformKeyStore {
    fn load(&self) -> Result<Option<Vec<u8>>, KeyStoreFailure> {
        match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, &self.account) {
            Ok(key) => Ok(Some(key)),
            // errSecItemNotFound.
            Err(error) if error.code() == -25300 => Ok(None),
            Err(error) => Err(Self::classify(error)),
        }
    }

    fn store(&self, key: &[u8]) -> Result<(), KeyStoreFailure> {
        security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, &self.account, key)
            .map_err(Self::classify)
    }
}

#[cfg(not(target_os = "macos"))]
struct PlatformKeyStore;

#[cfg(not(target_os = "macos"))]
impl PlatformKeyStore {
    fn for_data_dir(_data_dir: &Path) -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl VaultKeyStore for PlatformKeyStore {
    fn load(&self) -> Result<Option<Vec<u8>>, KeyStoreFailure> {
        Err(KeyStoreFailure::Unavailable)
    }

    fn store(&self, _key: &[u8]) -> Result<(), KeyStoreFailure> {
        Err(KeyStoreFailure::Unavailable)
    }
}

#[derive(Default)]
struct MemoryKeyStore {
    state: Mutex<MemoryKeyState>,
}

#[derive(Default)]
struct MemoryKeyState {
    key: Option<Vec<u8>>,
    failure: Option<KeyStoreFailure>,
}

impl VaultKeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<Vec<u8>>, KeyStoreFailure> {
        let state = lock(&self.state);
        if let Some(failure) = state.failure {
            Err(failure)
        } else {
            Ok(state.key.clone())
        }
    }

    fn store(&self, key: &[u8]) -> Result<(), KeyStoreFailure> {
        let mut state = lock(&self.state);
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        state.key = Some(key.to_vec());
        Ok(())
    }
}

#[derive(Clone)]
pub struct CredentialBroker {
    inner: Arc<BrokerInner>,
}

impl fmt::Debug for CredentialBroker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialBroker")
            .field("vault_file", &self.inner.vault_file)
            .field("key_store", &"[OS CREDENTIAL STORE]")
            .finish()
    }
}

struct BrokerInner {
    vault_file: PathBuf,
    key_store: Arc<dyn VaultKeyStore>,
    mutation: Mutex<()>,
}

impl CredentialBroker {
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::with_store(
            data_dir.join("credentials").join("environment-v1.vault"),
            Arc::new(PlatformKeyStore::for_data_dir(data_dir)),
        )
    }

    /// Hermetic broker for tests and isolated fixture profiles. The key exists
    /// only in process memory and is never a production fallback.
    #[doc(hidden)]
    pub fn in_memory_for_tests(data_dir: &Path) -> Self {
        Self::with_store(
            data_dir.join("credentials").join("environment-v1.vault"),
            Arc::new(MemoryKeyStore::default()),
        )
    }

    fn with_store(vault_file: PathBuf, key_store: Arc<dyn VaultKeyStore>) -> Self {
        Self {
            inner: Arc::new(BrokerInner {
                vault_file,
                key_store,
                mutation: Mutex::new(()),
            }),
        }
    }

    /// Add or replace one LUNA-owned value. No remote ingress is accepted in
    /// L05, and the returned object is metadata plus a scoped lease, never the
    /// raw value.
    pub fn store_local(
        &self,
        ingress: SecretIngress,
        scope: CredentialScope,
        name: &str,
        purpose: &str,
        destination: &str,
        value: &[u8],
    ) -> Result<(CredentialMetadata, CredentialLease), CredentialError> {
        if !matches!(ingress, SecretIngress::LocalEngine) {
            return Err(CredentialError::RemoteSecretFlowUnavailable);
        }
        validate_label(name)?;
        validate_label(purpose)?;
        validate_label(destination)?;
        if value.is_empty() {
            return Err(CredentialError::InvalidRequest(
                "credential values cannot be empty",
            ));
        }
        let _mutation = lock(&self.inner.mutation);
        let mut payload = self.load_or_empty_for_write()?;
        let now = now_ms();
        let existing_id = payload.entries.iter().find_map(|(id, entry)| {
            (entry.metadata.name == name && entry.metadata.scope == scope).then(|| id.clone())
        });
        let id = existing_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let prior = payload.entries.get(&id);
        let revision = prior
            .map(|entry| entry.metadata.revision.saturating_add(1))
            .unwrap_or(1);
        let metadata = CredentialMetadata {
            id: id.clone(),
            name: name.to_string(),
            scope: scope.clone(),
            purpose: purpose.to_string(),
            destination: destination.to_string(),
            state: CredentialState::Configured,
            revision,
            created_at: prior.map(|entry| entry.metadata.created_at).unwrap_or(now),
            updated_at: now,
            last_used_at: prior.and_then(|entry| entry.metadata.last_used_at),
        };
        payload.entries.insert(
            id.clone(),
            VaultEntry {
                metadata: metadata.clone(),
                value: Some(BASE64.encode(value)),
            },
        );
        payload.revision = payload.revision.saturating_add(1);
        self.persist(&payload)?;
        Ok((
            metadata,
            CredentialLease {
                id,
                revision,
                scope,
            },
        ))
    }

    pub fn list_metadata(&self) -> Result<Vec<CredentialMetadata>, CredentialError> {
        let _mutation = lock(&self.inner.mutation);
        let Some(payload) = self.load_existing()? else {
            return Ok(Vec::new());
        };
        Ok(payload
            .entries
            .values()
            .map(|entry| entry.metadata.clone())
            .collect())
    }

    pub fn lease_by_id(
        &self,
        id: &str,
        exact_scope: &CredentialScope,
    ) -> Result<CredentialLease, CredentialError> {
        let _mutation = lock(&self.inner.mutation);
        let payload = self.load_existing()?.ok_or(CredentialError::NotFound)?;
        let entry = payload.entries.get(id).ok_or(CredentialError::NotFound)?;
        if &entry.metadata.scope != exact_scope {
            return Err(CredentialError::ScopeDenied);
        }
        if entry.metadata.state != CredentialState::Configured || entry.value.is_none() {
            return Err(CredentialError::Revoked);
        }
        Ok(CredentialLease {
            id: id.to_string(),
            revision: entry.metadata.revision,
            scope: exact_scope.clone(),
        })
    }

    pub fn forget(
        &self,
        lease: &CredentialLease,
        exact_scope: &CredentialScope,
    ) -> Result<CredentialMetadata, CredentialError> {
        let _mutation = lock(&self.inner.mutation);
        let mut payload = self.load_existing()?.ok_or(CredentialError::NotFound)?;
        let entry = payload
            .entries
            .get_mut(&lease.id)
            .ok_or(CredentialError::NotFound)?;
        authorize(entry, lease, exact_scope)?;
        entry.value = None;
        entry.metadata.state = CredentialState::Forgotten;
        entry.metadata.revision = entry.metadata.revision.saturating_add(1);
        entry.metadata.updated_at = now_ms();
        let metadata = entry.metadata.clone();
        payload.revision = payload.revision.saturating_add(1);
        self.persist(&payload)?;
        Ok(metadata)
    }

    pub(crate) fn materialize(
        &self,
        lease: &CredentialLease,
        exact_scope: &CredentialScope,
    ) -> Result<SecretMaterial, CredentialError> {
        let _mutation = lock(&self.inner.mutation);
        let mut payload = self.load_existing()?.ok_or(CredentialError::NotFound)?;
        let entry = payload
            .entries
            .get_mut(&lease.id)
            .ok_or(CredentialError::NotFound)?;
        authorize(entry, lease, exact_scope)?;
        let encoded = entry.value.as_ref().ok_or(CredentialError::Revoked)?;
        let value = SecretMaterial(
            BASE64
                .decode(encoded)
                .map_err(|_| CredentialError::VaultCorrupt)?,
        );
        entry.metadata.last_used_at = Some(now_ms());
        entry.metadata.updated_at = now_ms();
        payload.revision = payload.revision.saturating_add(1);
        self.persist(&payload)?;
        Ok(value)
    }

    fn load_or_empty_for_write(&self) -> Result<VaultPayload, CredentialError> {
        Ok(self.load_existing()?.unwrap_or_else(VaultPayload::new))
    }

    fn load_existing(&self) -> Result<Option<VaultPayload>, CredentialError> {
        let bytes = match std::fs::read(&self.inner.vault_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(CredentialError::StorageUnavailable),
        };
        let key = self.load_key(true)?;
        let envelope: VaultEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| CredentialError::VaultCorrupt)?;
        if envelope.format != VAULT_FORMAT
            || envelope.version != VAULT_VERSION
            || envelope.cipher != VAULT_CIPHER
        {
            return Err(CredentialError::VaultCorrupt);
        }
        let nonce = BASE64
            .decode(envelope.nonce)
            .map_err(|_| CredentialError::VaultCorrupt)?;
        let ciphertext = BASE64
            .decode(envelope.ciphertext)
            .map_err(|_| CredentialError::VaultCorrupt)?;
        if nonce.len() != NONCE_BYTES {
            return Err(CredentialError::VaultCorrupt);
        }
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CredentialError::VaultCorrupt)?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: VAULT_AAD,
                    },
                )
                .map_err(|_| CredentialError::VaultCorrupt)?,
        );
        let payload: VaultPayload =
            serde_json::from_slice(&plaintext).map_err(|_| CredentialError::VaultCorrupt)?;
        if payload.version != VAULT_VERSION {
            return Err(CredentialError::VaultCorrupt);
        }
        Ok(Some(payload))
    }

    fn persist(&self, payload: &VaultPayload) -> Result<(), CredentialError> {
        let key = self.load_key(false)?;
        let plaintext =
            Zeroizing::new(serde_json::to_vec(payload).map_err(|_| CredentialError::VaultCorrupt)?);
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| CredentialError::CredentialStoreUnavailable)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CredentialError::VaultCorrupt)?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| CredentialError::VaultCorrupt)?;
        let envelope = VaultEnvelope {
            format: VAULT_FORMAT.into(),
            version: VAULT_VERSION,
            cipher: VAULT_CIPHER.into(),
            nonce: BASE64.encode(nonce),
            ciphertext: BASE64.encode(ciphertext),
        };
        let bytes = serde_json::to_vec(&envelope).map_err(|_| CredentialError::VaultCorrupt)?;
        write_atomic_private(&self.inner.vault_file, &bytes)
            .map_err(|_| CredentialError::StorageUnavailable)
    }

    /// `must_exist` is true when ciphertext is already present. Losing the key
    /// can never silently create a replacement key over an existing vault.
    fn load_key(&self, must_exist: bool) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
        match self.inner.key_store.load().map_err(map_key_store)? {
            Some(key) => {
                let key = Zeroizing::new(key);
                if key.len() == KEY_BYTES {
                    Ok(key)
                } else {
                    Err(CredentialError::VaultCorrupt)
                }
            }
            None if must_exist => Err(CredentialError::VaultKeyMissing),
            None => {
                let mut key = Zeroizing::new(vec![0u8; KEY_BYTES]);
                getrandom::fill(&mut key)
                    .map_err(|_| CredentialError::CredentialStoreUnavailable)?;
                self.inner.key_store.store(&key).map_err(map_key_store)?;
                Ok(key)
            }
        }
    }
}

pub(crate) struct SecretMaterial(Vec<u8>);

impl fmt::Debug for SecretMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretMaterial([REDACTED])")
    }
}

impl SecretMaterial {
    pub(crate) fn as_utf8(&self) -> Result<&str, CredentialError> {
        std::str::from_utf8(&self.0).map_err(|_| CredentialError::VaultCorrupt)
    }
}

impl Drop for SecretMaterial {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultEnvelope {
    format: String,
    version: u32,
    cipher: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultPayload {
    version: u32,
    revision: u64,
    entries: BTreeMap<String, VaultEntry>,
}

impl VaultPayload {
    fn new() -> Self {
        Self {
            version: VAULT_VERSION,
            revision: 0,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultEntry {
    metadata: CredentialMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

impl Drop for VaultEntry {
    fn drop(&mut self) {
        if let Some(value) = self.value.as_mut() {
            value.zeroize();
        }
    }
}

fn authorize(
    entry: &VaultEntry,
    lease: &CredentialLease,
    exact_scope: &CredentialScope,
) -> Result<(), CredentialError> {
    if &entry.metadata.scope != exact_scope || &lease.scope != exact_scope {
        return Err(CredentialError::ScopeDenied);
    }
    if entry.metadata.state != CredentialState::Configured
        || entry.metadata.revision != lease.revision
    {
        return Err(CredentialError::Revoked);
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<(), CredentialError> {
    if label.trim().is_empty() || label.len() > 200 || label.chars().any(char::is_control) {
        Err(CredentialError::InvalidRequest(
            "credential metadata must be non-empty, bounded text",
        ))
    } else {
        Ok(())
    }
}

fn map_key_store(error: KeyStoreFailure) -> CredentialError {
    match error {
        KeyStoreFailure::Unavailable => CredentialError::CredentialStoreUnavailable,
        KeyStoreFailure::Locked => CredentialError::CredentialStoreLocked,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn hex_prefix(bytes: &[u8], bytes_to_use: usize) -> String {
    bytes
        .iter()
        .take(bytes_to_use)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_atomic_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "vault path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    std::fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vault");
    let temp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let outcome = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker(root: &Path) -> CredentialBroker {
        CredentialBroker::in_memory_for_tests(root)
    }

    fn store_sync(
        broker: &CredentialBroker,
        value: &[u8],
    ) -> (CredentialMetadata, CredentialLease) {
        broker
            .store_local(
                SecretIngress::LocalEngine,
                CredentialScope::luna_sync(),
                "refresh-token",
                "Refresh the optional LUNA sync session",
                "LUNA edge authentication",
                value,
            )
            .unwrap()
    }

    #[test]
    fn encrypted_lifecycle_is_scoped_redacted_and_revocable() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path());
        let secret = b"synthetic-refresh-value-never-plaintext";
        let (metadata, lease) = store_sync(&broker, secret);

        let vault = std::fs::read(root.path().join("credentials/environment-v1.vault")).unwrap();
        assert!(!vault.windows(secret.len()).any(|window| window == secret));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let vault_mode =
                std::fs::metadata(root.path().join("credentials/environment-v1.vault"))
                    .unwrap()
                    .permissions()
                    .mode();
            let dir_mode = std::fs::metadata(root.path().join("credentials"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(vault_mode & 0o777, 0o600);
            assert_eq!(dir_mode & 0o777, 0o700);
        }
        let serialized = serde_json::to_string(&metadata).unwrap();
        assert!(!serialized.contains("synthetic-refresh"));
        assert_eq!(metadata.state, CredentialState::Configured);

        let wrong_scope = CredentialScope {
            class: CredentialClass::Integration,
            owner: "not-sync".into(),
            project_id: None,
        };
        assert_eq!(
            broker.materialize(&lease, &wrong_scope).unwrap_err(),
            CredentialError::ScopeDenied
        );
        assert_eq!(
            broker
                .materialize(&lease, &CredentialScope::luna_sync())
                .unwrap()
                .as_utf8()
                .unwrap(),
            std::str::from_utf8(secret).unwrap()
        );

        let forgotten = broker
            .forget(&lease, &CredentialScope::luna_sync())
            .unwrap();
        assert_eq!(forgotten.state, CredentialState::Forgotten);
        assert_eq!(
            broker
                .materialize(&lease, &CredentialScope::luna_sync())
                .unwrap_err(),
            CredentialError::Revoked
        );
        assert!(!format!("{forgotten:?}").contains("synthetic-refresh"));
    }

    #[test]
    fn replaced_value_invalidates_the_previous_lease() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path());
        let (_, old) = store_sync(&broker, b"first-synthetic-value");
        let (_, current) = store_sync(&broker, b"second-synthetic-value");
        assert_eq!(old.id, current.id);
        assert!(current.revision > old.revision);
        assert_eq!(
            broker
                .materialize(&old, &CredentialScope::luna_sync())
                .unwrap_err(),
            CredentialError::Revoked
        );
        assert_eq!(
            broker
                .materialize(&current, &CredentialScope::luna_sync())
                .unwrap()
                .as_utf8()
                .unwrap(),
            "second-synthetic-value"
        );
    }

    #[test]
    fn locked_and_unavailable_key_stores_fail_closed() {
        for failure in [KeyStoreFailure::Locked, KeyStoreFailure::Unavailable] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryKeyStore::default());
            lock(&store.state).failure = Some(failure);
            let broker = CredentialBroker::with_store(
                root.path().join("credentials/environment-v1.vault"),
                store,
            );
            let error = broker
                .store_local(
                    SecretIngress::LocalEngine,
                    CredentialScope::luna_sync(),
                    "refresh-token",
                    "fixture purpose",
                    "fixture destination",
                    b"fixture-value",
                )
                .unwrap_err();
            assert_eq!(error, map_key_store(failure));
            assert!(
                !root
                    .path()
                    .join("credentials/environment-v1.vault")
                    .exists()
            );
        }
    }

    #[test]
    fn remote_ingress_is_denied_before_storage() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path());
        let result = broker.store_local(
            SecretIngress::Remote {
                end_to_end_encrypted: true,
            },
            CredentialScope::luna_sync(),
            "refresh-token",
            "purpose",
            "destination",
            b"fixture-value",
        );
        assert_eq!(
            result.unwrap_err(),
            CredentialError::RemoteSecretFlowUnavailable
        );
        assert!(broker.list_metadata().unwrap().is_empty());
    }

    #[test]
    fn existing_ciphertext_with_a_missing_key_never_creates_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryKeyStore::default());
        let broker = CredentialBroker::with_store(
            root.path().join("credentials/environment-v1.vault"),
            store.clone(),
        );
        store_sync(&broker, b"fixture-value");
        let before = std::fs::read(root.path().join("credentials/environment-v1.vault")).unwrap();
        lock(&store.state).key = None;

        assert_eq!(
            broker.list_metadata().unwrap_err(),
            CredentialError::VaultKeyMissing
        );
        assert_eq!(
            std::fs::read(root.path().join("credentials/environment-v1.vault")).unwrap(),
            before
        );
        assert!(lock(&store.state).key.is_none());
    }

    #[test]
    fn corrupted_ciphertext_never_returns_material() {
        let root = tempfile::tempdir().unwrap();
        let broker = broker(root.path());
        let (_, lease) = store_sync(&broker, b"fixture-value");
        let path = root.path().join("credentials/environment-v1.vault");
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        envelope["ciphertext"] = serde_json::Value::String(BASE64.encode(b"tampered"));
        std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert_eq!(
            broker
                .materialize(&lease, &CredentialScope::luna_sync())
                .unwrap_err(),
            CredentialError::VaultCorrupt
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keychain_backs_an_isolated_fixture_vault() {
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = security_framework::passwords::delete_generic_password(
                    KEYCHAIN_SERVICE,
                    &self.0,
                );
            }
        }

        let root = tempfile::tempdir().unwrap();
        let store = PlatformKeyStore::for_data_dir(root.path());
        let _cleanup = Cleanup(store.account.clone());
        let broker = CredentialBroker::with_store(
            root.path().join("credentials/environment-v1.vault"),
            Arc::new(store),
        );
        let (_, lease) = store_sync(&broker, b"isolated-keychain-fixture-value");
        assert_eq!(
            broker
                .materialize(&lease, &CredentialScope::luna_sync())
                .unwrap()
                .as_utf8()
                .unwrap(),
            "isolated-keychain-fixture-value"
        );
        security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, &_cleanup.0)
            .unwrap();
        assert_eq!(
            security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, &_cleanup.0,)
                .unwrap_err()
                .code(),
            -25300
        );
    }
}
