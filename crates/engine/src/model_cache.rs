//! Device-owned, last-known-good model catalogs. Provider discovery never runs
//! on the startup path; the first picker can read this small snapshot while a
//! bounded background refresh runs after the engine is serving requests.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use futures::{StreamExt, stream};
use luna_proto::{HarnessId, Model};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_accounts::AgentAccounts;
use crate::device_write::DeviceWriteHandle;
use crate::registry::{HarnessRegistry, descriptor_enabled};

const CACHE_VERSION: u32 = 2;
const MAX_CACHE_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogFile {
    version: u32,
    checked_at_ms: i64,
    source_signature: Option<String>,
    models: Vec<Model>,
}

/// Bind Codex's cached list to the exact executable used for task execution.
/// Canonicalizing follows package-manager symlinks when an update replaces
/// their target. Metadata avoids spawning the CLI on the picker hot path.
fn codex_source_signature() -> Option<String> {
    let path = luna_harness::codex::resolve_codex_executable()?
        .canonicalize()
        .ok()?;
    let metadata = fs::metadata(&path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!(
        "{}:{}:{}",
        path.display(),
        metadata.len(),
        modified.as_nanos()
    ))
}

pub(crate) struct ModelCache {
    root: PathBuf,
    write: DeviceWriteHandle,
    registry: Arc<HarnessRegistry>,
    accounts: AgentAccounts,
    in_flight: tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    bootstrap_started: AtomicBool,
}

impl ModelCache {
    pub(crate) fn new(
        root: PathBuf,
        write: DeviceWriteHandle,
        registry: Arc<HarnessRegistry>,
        accounts: AgentAccounts,
    ) -> Self {
        Self {
            root,
            write,
            registry,
            accounts,
            in_flight: tokio::sync::Mutex::new(HashMap::new()),
            bootstrap_started: AtomicBool::new(false),
        }
    }

    fn path(&self, harness: HarnessId, identity: &str) -> PathBuf {
        let key = format!("{harness:?}:{identity}");
        let digest = Sha256::digest(key.as_bytes());
        self.root
            .join("model-cache")
            .join(format!("{digest:x}.json"))
    }

    fn read(&self, harness: HarnessId, identity: &str) -> Option<Vec<Model>> {
        let path = self.path(harness, identity);
        let root = self.root.canonicalize().ok()?;
        if path.parent()?.canonicalize().ok()? != root.join("model-cache") {
            return None;
        }
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_CACHE_BYTES {
            return None;
        }
        let file = fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_CACHE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            return None;
        }
        let catalog: CatalogFile = serde_json::from_slice(&bytes).ok()?;
        let source_matches = harness != HarnessId::Codex
            || codex_source_signature().as_deref() == catalog.source_signature.as_deref();
        (catalog.version == CACHE_VERSION
            && source_matches
            && (!catalog.models.is_empty() || harness == HarnessId::Codex))
            .then_some(catalog.models)
    }

    fn save(&self, harness: HarnessId, identity: &str, models: &[Model]) -> Result<(), String> {
        let _permit = self
            .write
            .try_admit()
            .map_err(|error| format!("{error:?}"))?;
        let root = self
            .root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if root != _permit.ticket().root() {
            return Err("model cache is outside the active device root".into());
        }
        let dir = root.join("model-cache");
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        if dir.canonicalize().map_err(|error| error.to_string())? != dir {
            return Err("model cache directory is not inside the device root".into());
        }
        let catalog = CatalogFile {
            version: CACHE_VERSION,
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            source_signature: (harness == HarnessId::Codex)
                .then(codex_source_signature)
                .flatten(),
            models: models.to_vec(),
        };
        if harness == HarnessId::Codex && catalog.source_signature.is_none() {
            return Err("Codex CLI executable is unavailable".into());
        }
        let bytes = serde_json::to_vec(&catalog).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            return Err("provider model catalog exceeds the cache size limit".into());
        }
        let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(|error| error.to_string())?;
        temp.write_all(&bytes).map_err(|error| error.to_string())?;
        temp.flush().map_err(|error| error.to_string())?;
        temp.persist(self.path(harness, identity))
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) async fn list(
        &self,
        harness: HarnessId,
        account_id: Option<&str>,
        refresh: bool,
    ) -> Result<Vec<Model>, String> {
        let codex_execution = if harness == HarnessId::Codex {
            Some(
                self.accounts
                    .resolve_execution(harness, account_id)
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let claude_execution = (harness == HarnessId::ClaudeCode)
            .then(|| {
                self.accounts
                    .resolve_execution(HarnessId::ClaudeCode, None)
                    .ok()
            })
            .flatten();
        let identity = codex_execution
            .as_ref()
            .or(claude_execution.as_ref())
            .map_or("device", |execution| execution.auth_instance_id.as_str());
        if !refresh && let Some(models) = self.read(harness, identity) {
            return Ok(models);
        }
        // A key can be probed by startup and Settings at once. Serialize its
        // provider calls without holding the map lock over network work.
        let gate = {
            let mut in_flight = self.in_flight.lock().await;
            in_flight
                .entry(self.path(harness, identity))
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _probe = gate.lock().await;
        if !refresh && let Some(models) = self.read(harness, identity) {
            return Ok(models);
        }

        let fetched = if let Some(execution) = codex_execution.as_ref() {
            let home = execution
                .codex_home
                .as_deref()
                .ok_or_else(|| "Codex account has no execution home".to_string())?;
            luna_harness::codex::CodexHarness::new()
                .live_models_for_home(home, execution.codex_file_auth)
                .await
                .map_err(|error| error.to_string())
        } else if harness == HarnessId::ClaudeCode {
            luna_harness::claude::ClaudeHarness::new()
                .live_models()
                .await
                .map_err(|error| error.to_string())
        } else {
            let provider = self
                .registry
                .resolve(harness)
                .map_err(|error| error.to_string())?;
            provider
                .models_live()
                .await
                .map_err(|error| error.to_string())
        };
        match fetched {
            Ok(models) if harness == HarnessId::Codex && models.is_empty() => {
                // An authoritative empty `model/list` must replace an older
                // runnable catalog for this account and CLI.
                self.save(harness, identity, &models)?;
                Ok(models)
            }
            Ok(models) if !models.is_empty() => {
                if let Err(error) = self.save(harness, identity, &models) {
                    tracing::warn!(?harness, %error, "model catalog could not be cached");
                    if refresh {
                        return Err(format!(
                            "model catalog loaded but could not be saved: {error}"
                        ));
                    }
                }
                Ok(models)
            }
            Ok(_) if !refresh && harness == HarnessId::ClaudeCode => {
                Ok(luna_harness::claude::catalog::static_models())
            }
            Err(error) if !refresh && harness == HarnessId::ClaudeCode => {
                tracing::debug!(%error, "Claude model discovery failed; using bundled fallback");
                Ok(luna_harness::claude::catalog::static_models())
            }
            Ok(_) => Err("provider returned an empty model catalog".into()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn refresh_after_provider_update(
        &self,
        harness: HarnessId,
    ) -> Result<(), String> {
        if harness == HarnessId::Codex {
            for account_id in self.accounts.codex_account_ids() {
                self.list(harness, Some(&account_id), true).await?;
            }
        } else {
            self.list(harness, None, true).await?;
        }
        Ok(())
    }

    /// Starts once per engine, after an RPC service exists. Two probes at most
    /// run concurrently; a slow or unavailable provider cannot block startup.
    pub(crate) fn start_background_refresh(self: &Arc<Self>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.bootstrap_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let cache = self.clone();
        runtime.spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            loop {
            let mut targets = Vec::new();
            for descriptor in cache.registry.descriptors() {
                if !descriptor.installed
                    || !descriptor_enabled(&descriptor)
                    || descriptor.id == HarnessId::Mock
                {
                    continue;
                }
                if descriptor.id == HarnessId::Codex {
                    targets.extend(
                        cache
                            .accounts
                            .codex_account_ids()
                            .into_iter()
                            .map(|id| (HarnessId::Codex, Some(id))),
                    );
                } else {
                    targets.push((descriptor.id, None));
                }
            }
            stream::iter(targets)
                .for_each_concurrent(2, |(harness, account_id)| {
                    let cache = cache.clone();
                    async move {
                        if let Err(error) = cache.list(harness, account_id.as_deref(), true).await {
                            tracing::debug!(?harness, %error, "background model refresh unavailable");
                        }
                    }
                })
                .await;
                tokio::time::sleep(Duration::from_secs(2 * 60 * 60)).await;
            }
        });
    }
}
