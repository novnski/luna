//! Profile-owned TLS identity of the direct host.
//!
//! An identity directory holds exactly two files:
//! - `tls-key.pem`: the P-256 private key, owner-only (`0600`);
//! - `target.json`: the [`RemoteTarget`] this key speaks for.
//!
//! The pin is the SHA-256 of the key's SubjectPublicKeyInfo. No certificate is
//! stored: a short self-signed certificate is derived from the key at every
//! start, so renewal never changes the pin and a key/certificate mismatch
//! cannot exist. A damaged or partial identity is refused rather than
//! regenerated, because regenerating would silently change the pin.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_proto::remote_control::RemoteTarget;
use rcgen::{CertificateParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

const KEY_FILE: &str = "tls-key.pem";
const TARGET_FILE: &str = "target.json";
const MAX_FILE_BYTES: u64 = 16 * 1024;
/// Derived certificates are valid from an hour ago for 397 days.
const CERT_DAYS: i64 = 397;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity is incomplete ({0} is missing); refusing to create a different key")]
    Incomplete(&'static str),
    #[error("{0} is not a regular file or directory of the expected kind")]
    WrongKind(PathBuf),
    #[error("{0} is accessible by other users")]
    TooPermissive(PathBuf),
    #[error("identity key is not a P-256 PKCS#8 key: {0}")]
    BadKey(String),
    #[error("identity target is invalid: {0}")]
    BadTarget(String),
    #[error("identity storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS setup failed: {0}")]
    Tls(String),
}

pub struct HostIdentity {
    target: RemoteTarget,
    key: KeyPair,
    spki_sha256: String,
}

impl HostIdentity {
    /// Open the identity in `dir`. Creates it with `target` only when neither
    /// file exists; any other partial state is refused.
    pub fn open_or_create(
        dir: &Path,
        target: impl FnOnce() -> RemoteTarget,
    ) -> Result<Self, IdentityError> {
        let key_path = dir.join(KEY_FILE);
        let target_path = dir.join(TARGET_FILE);
        match (present(&key_path), present(&target_path)) {
            (false, false) => Self::create(dir, target()),
            (true, true) => Self::open(dir, &key_path, &target_path),
            (false, true) => Err(IdentityError::Incomplete(KEY_FILE)),
            (true, false) => Err(IdentityError::Incomplete(TARGET_FILE)),
        }
    }

    pub fn target(&self) -> &RemoteTarget {
        &self.target
    }

    /// Lowercase hex SHA-256 of the SubjectPublicKeyInfo (DER).
    pub fn spki_sha256(&self) -> &str {
        &self.spki_sha256
    }

    /// A TLS server configuration presenting a certificate freshly derived
    /// from the identity key. HTTP/1.1 only.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, IdentityError> {
        let tls = |error: &dyn std::fmt::Display| IdentityError::Tls(error.to_string());
        let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| tls(&e))?;
        params.distinguished_name.push(
            DnType::CommonName,
            format!("LUNA host {}", self.target.device_id),
        );
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::hours(1);
        params.not_after = now + time::Duration::days(CERT_DAYS);
        let certificate = params.self_signed(&self.key).map_err(|e| tls(&e))?;
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.serialize_der()));
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| tls(&e))?
        .with_no_client_auth()
        .with_single_cert(vec![certificate.der().clone()], key)
        .map_err(|e| tls(&e))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    fn create(dir: &Path, target: RemoteTarget) -> Result<Self, IdentityError> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        private_dir(dir)?;
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|error| IdentityError::BadKey(error.to_string()))?;
        let target_json = serde_json::to_vec_pretty(&target)
            .map_err(|error| IdentityError::BadTarget(error.to_string()))?;
        // The target first: a crash before the key leaves an incomplete
        // identity, which the next start refuses instead of regenerating.
        write_new(&dir.join(TARGET_FILE), &target_json, 0o600)?;
        write_new(&dir.join(KEY_FILE), key.serialize_pem().as_bytes(), 0o600)?;
        File::open(dir)?.sync_all()?;
        Ok(Self::from_parts(target, key))
    }

    fn open(dir: &Path, key_path: &Path, target_path: &Path) -> Result<Self, IdentityError> {
        private_dir(dir)?;
        let key_pem = read_private(key_path)?;
        let target_json = read_private(target_path)?;
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|error| IdentityError::BadKey(error.to_string()))?;
        if key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
            return Err(IdentityError::BadKey("not an ECDSA P-256 key".into()));
        }
        let target = serde_json::from_str::<RemoteTarget>(&target_json)
            .map_err(|error| IdentityError::BadTarget(error.to_string()))?;
        Ok(Self::from_parts(target, key))
    }

    fn from_parts(target: RemoteTarget, key: KeyPair) -> Self {
        let digest = Sha256::digest(key.subject_public_key_info());
        let spki_sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            target,
            key,
            spki_sha256,
        }
    }
}

/// A path that exists in any form, including a dangling symlink.
fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn private_dir(dir: &Path) -> Result<(), IdentityError> {
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.file_type().is_dir() {
        return Err(IdentityError::WrongKind(dir.to_owned()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(IdentityError::TooPermissive(dir.to_owned()));
    }
    Ok(())
}

fn read_private(path: &Path) -> Result<String, IdentityError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(IdentityError::WrongKind(path.to_owned()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(IdentityError::TooPermissive(path.to_owned()));
    }
    let mut text = String::new();
    File::open(path)?
        .take(MAX_FILE_BYTES)
        .read_to_string(&mut text)?;
    Ok(text)
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), IdentityError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| match error.kind() {
            ErrorKind::AlreadyExists => IdentityError::WrongKind(path.to_owned()),
            _ => IdentityError::Io(error),
        })?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
