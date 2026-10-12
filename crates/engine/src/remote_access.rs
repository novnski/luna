//! Direct remote access to this engine (RM-01, "Control this Mac").
//!
//! Off by default. A local-only operation enables it on one explicit LAN
//! interface; the choice is persisted per profile in `remote-access.json`,
//! and the TLS identity lives in `remote-identity/`, both beside the
//! profile's `remote-auth.json`. While enabled, the engine's restricted
//! remote service is served through [`luna_remote::gateway::Gateway`].
//! Disabling refuses new admissions first, then closes every socket.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use luna_proto::remote_control::RemoteTarget;
use luna_rpc::{RemoteAuthStore, RpcService};
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "remote-access.json";
#[cfg(unix)]
const IDENTITY_DIR: &str = "remote-identity";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bind: Option<String>,
}

/// What `GetRemoteAccess` and `SetRemoteAccess` report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessStatus {
    pub enabled: bool,
    pub bind: Option<String>,
    pub listening: bool,
    pub address: Option<String>,
    pub spki_sha256: Option<String>,
    pub target: Option<RemoteTarget>,
    /// Why an enabled gateway is not listening.
    pub error: Option<String>,
}

pub(crate) struct RemoteAccess {
    root: PathBuf,
    auth: Option<Arc<RemoteAuthStore>>,
    /// The engine's restricted remote service, attached once it exists.
    service: OnceLock<Arc<dyn RpcService>>,
    /// Serializes enable, disable, resume and stop.
    #[cfg(unix)]
    running: tokio::sync::Mutex<Option<luna_remote::gateway::Gateway>>,
    last_error: Mutex<Option<String>>,
}

impl RemoteAccess {
    pub(crate) fn new(root: &Path, auth: Option<Arc<RemoteAuthStore>>) -> Arc<Self> {
        Arc::new(Self {
            root: root.to_path_buf(),
            auth,
            service: OnceLock::new(),
            #[cfg(unix)]
            running: tokio::sync::Mutex::new(None),
            last_error: Mutex::new(None),
        })
    }

    pub(crate) fn attach_service(&self, service: Arc<dyn RpcService>) {
        let _ = self.service.set(service);
    }

    pub(crate) async fn status(&self) -> RemoteAccessStatus {
        let config = self.config().unwrap_or_default();
        let mut status = RemoteAccessStatus {
            enabled: config.enabled,
            bind: config.bind,
            error: self
                .last_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            ..Default::default()
        };
        #[cfg(unix)]
        if let Some(gateway) = self.running.lock().await.as_ref() {
            status.listening = true;
            status.address = Some(gateway.address().to_string());
            status.spki_sha256 = Some(gateway.spki_sha256().to_owned());
            status.target = Some(gateway.target().clone());
            status.error = None;
        }
        status
    }

    /// Enable on `bind` (or the persisted address), or disable. The change is
    /// persisted only after it took effect.
    pub(crate) async fn set(
        &self,
        enabled: bool,
        bind: Option<String>,
    ) -> Result<RemoteAccessStatus, String> {
        let mut config = self.config()?;
        if !enabled {
            self.stop().await;
            config.enabled = false;
            self.save(&config)?;
            self.set_error(None);
            return Ok(self.status().await);
        }
        let bind = bind
            .or(config.bind.clone())
            .ok_or("Choose the network address and port to listen on.")?;
        self.start(&bind).await?;
        config.enabled = true;
        config.bind = Some(bind);
        self.save(&config)?;
        Ok(self.status().await)
    }

    /// Start the persisted gateway, if remote access was left enabled.
    pub(crate) async fn resume(&self) {
        let config = match self.config() {
            Ok(config) => config,
            Err(error) => return self.set_error(Some(error)),
        };
        if let (true, Some(bind)) = (config.enabled, config.bind)
            && let Err(error) = self.start(&bind).await
        {
            tracing::error!(%error, "remote access did not resume");
        }
    }

    /// Engine shutdown or disable: refuse admission, then close everything.
    pub(crate) async fn stop(&self) {
        #[cfg(unix)]
        if let Some(gateway) = self.running.lock().await.take() {
            gateway.stop().await;
        }
    }

    /// The engine was dropped without shutdown: stop without waiting.
    pub(crate) fn abort(&self) {
        #[cfg(unix)]
        if let Ok(mut running) = self.running.try_lock() {
            running.take();
        }
    }

    #[cfg(unix)]
    async fn start(&self, bind: &str) -> Result<(), String> {
        let result = self.try_start(bind).await;
        self.set_error(result.as_ref().err().cloned());
        result
    }

    #[cfg(unix)]
    async fn try_start(&self, bind: &str) -> Result<(), String> {
        let address: std::net::SocketAddr = bind
            .parse()
            .map_err(|_| format!("{bind} is not an address:port"))?;
        let auth = self
            .auth
            .clone()
            .ok_or("The remote authority store is unavailable; see `luna remote sessions`.")?;
        let service = self
            .service
            .get()
            .cloned()
            .ok_or("The engine's remote service is not ready yet.")?;
        let mut running = self.running.lock().await;
        if let Some(gateway) = running.as_ref()
            && gateway.address() == address
        {
            return Ok(());
        }
        if let Some(previous) = running.take() {
            previous.stop().await;
        }
        let target = auth.target();
        let identity = luna_remote::identity::HostIdentity::open_or_create(
            &self.root.join(IDENTITY_DIR),
            || target,
        )
        .map_err(|error| format!("Remote identity: {error}"))?;
        let gateway = luna_remote::gateway::Gateway::start(
            address,
            luna_remote::gateway::GatewayParts {
                identity,
                auth,
                service,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        tracing::info!(address = %gateway.address(), "remote access listening");
        *running = Some(gateway);
        Ok(())
    }

    #[cfg(not(unix))]
    async fn start(&self, _bind: &str) -> Result<(), String> {
        Err("Remote access is not available on this platform.".into())
    }

    fn set_error(&self, error: Option<String>) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = error;
    }

    fn config(&self) -> Result<Config, String> {
        let path = self.root.join(CONFIG_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{} is unreadable: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// Private, published atomically.
    fn save(&self, config: &Config) -> Result<(), String> {
        let failed = |error: std::io::Error| format!("saving remote access: {error}");
        std::fs::create_dir_all(&self.root).map_err(failed)?;
        let path = self.root.join(CONFIG_FILE);
        let temp = self
            .root
            .join(format!(".{CONFIG_FILE}.{}.tmp", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = (|| {
            use std::io::Write;
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec(config).expect("config serializes"))?;
            file.sync_all()?;
            std::fs::rename(&temp, &path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        written.map_err(failed)
    }
}
