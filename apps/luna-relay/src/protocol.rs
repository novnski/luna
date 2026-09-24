use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use getrandom::fill;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeron_rpc::remote_auth::{PROTOCOL_VERSION as RPC_VERSION, RemoteTarget};

pub const VERSION: u16 = 1;
pub const MAX_ROUTES: usize = 32;
pub const MAX_CLIENT_KEYS: usize = 32;
pub const MAX_ACTIVE_PER_ROUTE: usize = 16;
pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;
pub const HANDSHAKE_SECS: u64 = 5;
pub const BRIDGE_SECS: u64 = 8;
pub const HEARTBEAT_SECS: u64 = 15;

pub type Target = RemoteTarget;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientKey {
    pub id: String,
    pub secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteConfig {
    pub id: String,
    pub target: Target,
    pub host_secret: String,
    pub clients: Vec<ClientKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayConfig {
    pub version: u16,
    pub routes: Vec<RouteConfig>,
}

impl RelayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        private_regular_file(path)?;
        let raw = fs::read(path).context("read private relay config")?;
        if raw.len() > 128 * 1024 {
            bail!("relay config exceeds 128 KiB");
        }
        let config: Self = serde_json::from_slice(&raw).context("parse relay config")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION || self.routes.len() > MAX_ROUTES {
            bail!("unsupported relay config version or route count");
        }
        let mut routes = std::collections::HashSet::new();
        let mut secrets = std::collections::HashSet::new();
        for route in &self.routes {
            validate_id(&route.id)?;
            if !routes.insert(&route.id) || route.clients.len() > MAX_CLIENT_KEYS {
                bail!("duplicate route or too many client keys");
            }
            if route.target.protocol_version != RPC_VERSION
                || route.target.device_id.is_empty()
                || route.target.environment_id.is_empty()
            {
                bail!("invalid exact L29 target");
            }
            validate_secret(&route.host_secret)?;
            if !secrets.insert(&route.host_secret) {
                bail!("relay credentials must be unique across roles and routes");
            }
            let mut clients = std::collections::HashSet::new();
            for client in &route.clients {
                validate_id(&client.id)?;
                validate_secret(&client.secret)?;
                if !clients.insert(&client.id) || !secrets.insert(&client.secret) {
                    bail!("duplicate client or reused relay credential");
                }
            }
        }
        Ok(())
    }

    pub fn route(&self, id: &str) -> Option<&RouteConfig> {
        self.routes.iter().find(|route| route.id == id)
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("route and client IDs must be 1-64 ASCII letters, digits, '_' or '-'");
    }
    Ok(())
}

pub fn validate_secret(secret: &str) -> Result<()> {
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("relay secrets must be 32 random bytes encoded as 64 hex characters");
    }
    Ok(())
}

pub fn read_private_secret(path: &Path) -> Result<String> {
    private_regular_file(path)?;
    let raw = fs::read_to_string(path).context("read private relay secret")?;
    if raw.len() > 128 {
        bail!("relay secret file is too large");
    }
    let secret = raw.trim().to_owned();
    validate_secret(&secret)?;
    Ok(secret)
}

pub fn private_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("stat private relay file")?;
    if !metadata.file_type().is_file() {
        bail!("relay file must be a regular file, not a symlink");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("relay file must be mode 0600 or stricter");
        }
    }
    Ok(())
}

pub fn random_hex() -> Result<String> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes).map_err(|error| anyhow::anyhow!("secure random failed: {error}"))?;
    Ok(hex(&bytes))
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

/// HMAC-SHA256 over one fresh server challenge, route, role, exact target and
/// bridge generation. The secret itself never appears in a URL or wire frame.
pub fn proof(
    secret: &str,
    challenge: &str,
    path: &str,
    target: &Target,
    generation: u64,
) -> Result<String> {
    validate_secret(secret)?;
    if challenge.len() != 64 || !challenge.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid relay challenge");
    }
    let mut key = [0_u8; 64];
    key[..secret.len()].copy_from_slice(secret.as_bytes());
    let mut inner = [0x36_u8; 64];
    let mut outer = [0x5c_u8; 64];
    for index in 0..64 {
        inner[index] ^= key[index];
        outer[index] ^= key[index];
    }
    let target = serde_json::to_vec(target)?;
    let mut digest = Sha256::new();
    digest.update(inner);
    digest.update(challenge.as_bytes());
    digest.update(path.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update(target);
    let inner_hash = digest.finalize();
    let mut digest = Sha256::new();
    digest.update(outer);
    digest.update(inner_hash);
    Ok(hex(&digest.finalize()))
}

pub fn proof_matches(expected: &str, supplied: &str) -> bool {
    if expected.len() != supplied.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(supplied.as_bytes())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Control {
    Challenge {
        nonce: String,
    },
    Hello {
        version: u16,
        target: Target,
        client_id: Option<String>,
        generation: u64,
        proof: String,
    },
    Ready {
        generation: u64,
    },
    Open {
        stream_id: String,
        generation: u64,
    },
    Rejected {
        reason: String,
    },
}
