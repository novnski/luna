//! Engine-owned L17 control for explicit copy-isolated backup roots. This is
//! deliberately separate from the live engine profile: until a write barrier
//! and normal-root writer coordination exist, normal roots cannot be backed up here.

#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(unix)]
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use zeroize::{Zeroize, Zeroizing};
use zeron_proto::{
    BackupArchiveRequest, BackupAvailability, BackupConfigureRequest, BackupPreview,
    BackupStageStatus, BackupStatus, BackupSummary,
};

use crate::backup::{
    BackupError, BackupReceipt, BackupService, IsolatedRoot, RecoveryKey, StagedEvidence,
    retention_candidates, validate_isolated_destination,
};
use crate::credentials::{
    CredentialBroker, CredentialError, CredentialScope, CredentialState, SecretIngress,
};

const STATE_FILE: &str = "backup-control-v1.json";
const KEY_NAME: &str = "LUNA backup recovery key";
const RESTORE_REASON: &str = "Restore activation remains unavailable: chat2 remote frontier, provider mappings, usage reconstruction, and post-start engine readback are unresolved.";

#[derive(Debug, thiserror::Error)]
pub enum BackupControlError {
    #[error("backup: {0}")]
    Backup(#[from] BackupError),
    #[error("credential: {0}")]
    Credential(#[from] CredentialError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("backup configuration is incomplete")]
    Unconfigured,
    #[error("recovery key must be exactly 64 hexadecimal characters")]
    InvalidRecoveryKey,
    #[error("choose exactly one recovery key source")]
    InvalidKeyConfiguration,
    #[error("production backups require an owner-held recovery key file; Keychain is not used")]
    KeyFileRequired,
    #[error(
        "recovery key file must be an owner-owned, mode-0600 regular file containing exactly 32 bytes"
    )]
    InvalidKeyFile,
    #[error("recovery key generation is limited to an explicit in-memory synthetic broker")]
    SyntheticKeyOnly,
    #[error("backup source, destination, and engine roots must be separate")]
    OverlappingRoots,
    #[error("backup control state is incompatible")]
    IncompatibleState,
    #[error("backup key replacement requires an explicit archive rotation plan")]
    KeyRotationRequiresPlan,
    #[error("changing backup roots requires an explicit archive migration plan")]
    ConfigChangeRequiresPlan,
    #[error("a staged restore already exists and requires review")]
    StageAlreadyExists,
    #[error("this staged restore predates restart-safe evidence and requires owner review")]
    StageNeedsReview,
}

type Result<T> = std::result::Result<T, BackupControlError>;

#[derive(Clone)]
pub struct BackupControl {
    inner: Arc<ControlInner>,
}

struct ControlInner {
    owner_root: IsolatedRoot,
    broker: CredentialBroker,
    operation: Mutex<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupConfig {
    source_root: PathBuf,
    destination_dir: PathBuf,
    automatic_enabled: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlState {
    version: u32,
    config: Option<BackupConfig>,
    #[serde(default)]
    recovery_key_file: Option<PathBuf>,
    receipts: Vec<BackupReceipt>,
    #[serde(default)]
    staged_restore: Option<BackupStageStatus>,
    #[serde(default)]
    staged_evidence: Option<StagedEvidence>,
    last_error: Option<String>,
}

impl BackupControl {
    /// Construct only from an explicitly marked synthetic/copy-isolated engine
    /// root. Passing a production broker is safe here; no vault or Keychain
    /// read occurs before an explicit configured operation.
    pub fn new(owner_root: IsolatedRoot, broker: CredentialBroker) -> Self {
        Self {
            inner: Arc::new(ControlInner {
                owner_root,
                broker,
                operation: Mutex::new(()),
            }),
        }
    }

    /// Generate owner-held recovery material only for marked synthetic
    /// fixtures with an in-memory broker. The production UI/RPC does not
    /// expose this method or fall back to the OS credential store.
    #[doc(hidden)]
    pub fn generate_recovery_key_for_synthetic_tests(&self) -> Result<Zeroizing<String>> {
        if !self.inner.broker.is_in_memory_for_tests() {
            return Err(BackupControlError::SyntheticKeyOnly);
        }
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *bytes)
            .map_err(|error| BackupError::Invalid(format!("randomness unavailable: {error}")))?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut owner_code = Zeroizing::new(String::with_capacity(64));
        for byte in bytes.iter() {
            owner_code.push(HEX[(byte >> 4) as usize] as char);
            owner_code.push(HEX[(byte & 0x0f) as usize] as char);
        }
        self.configure_recovery_key_hex(owner_code.to_string())?;
        Ok(owner_code)
    }

    /// Legacy in-memory fixture ingress. Production uses an owner-held key
    /// file instead of this broker path, so this never opens Keychain.
    pub fn configure_recovery_key_hex(&self, mut hex: String) -> Result<()> {
        if !self.inner.broker.is_in_memory_for_tests() {
            hex.zeroize();
            return Err(BackupControlError::KeyFileRequired);
        }
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let state = self.load_state();
        let state = match state {
            Ok(state) => state,
            Err(error) => {
                hex.zeroize();
                return Err(error);
            }
        };
        if state.recovery_key_file.is_some() {
            hex.zeroize();
            return Err(BackupControlError::InvalidKeyConfiguration);
        }
        if !state.receipts.is_empty() {
            hex.zeroize();
            return Err(BackupControlError::KeyRotationRequiresPlan);
        }
        let parsed = parse_key(&hex);
        hex.zeroize();
        let key = Zeroizing::new(parsed?);
        self.inner.broker.store_local(
            SecretIngress::LocalEngine,
            CredentialScope::luna_backup(),
            KEY_NAME,
            "Encrypt and decrypt LUNA backup archives",
            "Copy-isolated LUNA backup service",
            key.as_ref(),
        )?;
        Ok(())
    }

    /// Select an existing owner-held 32-byte key file. The file stays outside
    /// the engine, backup source, and destination roots; LUNA stores only its
    /// canonical path and never opens Keychain for this backup key.
    pub fn configure_recovery_key_file(&self, path: String) -> Result<()> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.load_state()?;
        if !state.receipts.is_empty() || state.staged_restore.is_some() {
            return Err(BackupControlError::KeyRotationRequiresPlan);
        }
        let file = Path::new(&path);
        if !file.is_absolute() || key_file_is_in_provider_home(file) {
            return Err(BackupControlError::InvalidKeyFile);
        }
        if fs::symlink_metadata(file)?.file_type().is_symlink() {
            return Err(BackupControlError::InvalidKeyFile);
        }
        let canonical = file.canonicalize()?;
        let _key = self.read_owner_key_file(&canonical, state.config.as_ref())?;
        state.version = 1;
        state.recovery_key_file = Some(canonical);
        self.save_state(&state)
    }

    pub fn configure(&self, request: BackupConfigureRequest) -> Result<BackupStatus> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let source = IsolatedRoot::open(&request.source_root)?;
        let destination = Path::new(&request.destination_dir);
        validate_isolated_destination(destination)?;
        let destination = destination.canonicalize()?;
        let owner = self.inner.owner_root.path();
        if source.path() == owner
            || source.path().starts_with(owner)
            || owner.starts_with(source.path())
            || destination.starts_with(source.path())
            || source.path().starts_with(&destination)
            || destination.starts_with(owner)
            || owner.starts_with(&destination)
        {
            return Err(BackupControlError::OverlappingRoots);
        }
        let mut state = self.load_state()?;
        state.version = 1;
        let next_config = BackupConfig {
            source_root: source.path().to_path_buf(),
            destination_dir: destination,
            automatic_enabled: request.automatic_enabled,
        };
        if let Some(key_file) = state.recovery_key_file.as_deref() {
            let _key = self.read_owner_key_file(key_file, Some(&next_config))?;
        }
        if state.config.as_ref().is_some_and(|prior| {
            prior.source_root != next_config.source_root
                || prior.destination_dir != next_config.destination_dir
        }) && (!state.receipts.is_empty() || state.staged_restore.is_some())
        {
            return Err(BackupControlError::ConfigChangeRequiresPlan);
        }
        state.config = Some(next_config);
        state.last_error = None;
        self.save_state(&state)?;
        self.status_from_state(state)
    }

    pub fn status(&self) -> Result<BackupStatus> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.status_from_state(self.load_state()?)
    }

    pub fn manual_backup(&self) -> Result<BackupSummary> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.load_state()?;
        let config = state
            .config
            .as_ref()
            .ok_or(BackupControlError::Unconfigured)?;
        let service = self.service(config)?;
        let receipt = service.backup_now(&config.destination_dir, crate::now_ms())?;
        let summary = summary(&receipt);
        state.receipts.push(receipt);
        state.last_error = None;
        self.save_state(&state)?;
        Ok(summary)
    }

    pub fn export(&self, request: BackupArchiveRequest) -> Result<BackupSummary> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.load_state()?;
        let config = state
            .config
            .as_ref()
            .ok_or(BackupControlError::Unconfigured)?;
        let service = self.service(config)?;
        let receipt = service.export(Path::new(&request.archive_path))?;
        let summary = summary(&receipt);
        state.receipts.push(receipt);
        state.last_error = None;
        self.save_state(&state)?;
        Ok(summary)
    }

    pub fn inspect(&self, request: BackupArchiveRequest) -> Result<BackupPreview> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let state = self.load_state()?;
        let config = state
            .config
            .as_ref()
            .ok_or(BackupControlError::Unconfigured)?;
        let service = self.service(config)?;
        let preview = service.preview_restore(Path::new(&request.archive_path))?;
        Ok(BackupPreview {
            archive_path: request.archive_path,
            source_root: preview.manifest.source_root,
            created_at_ms: preview.manifest.created_at_ms,
            total_bytes: preview.required_bytes,
            entry_count: preview.manifest.entries.len(),
            database_count: preview.manifest.databases.len(),
            document_count: preview
                .manifest
                .databases
                .iter()
                .map(|db| db.documents)
                .sum(),
            task_event_count: preview
                .manifest
                .databases
                .iter()
                .map(|db| db.task_events)
                .sum(),
            attachment_count: preview
                .manifest
                .entries
                .iter()
                .filter(|e| e.category == "attachment")
                .count(),
            excluded_categories: preview.manifest.excluded_categories,
            conflicts: preview.conflicts,
            unsupported: preview.unsupported,
            restore_activation_available: false,
        })
    }

    /// Verify and stage an archive in the separate copy-isolated root. A
    /// repeat request for the same archive rechecks the persisted stage after
    /// restart; a different archive needs explicit owner resolution.
    pub fn stage_restore(&self, request: BackupArchiveRequest) -> Result<BackupStageStatus> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.load_state()?;
        let config = state
            .config
            .as_ref()
            .ok_or(BackupControlError::Unconfigured)?;
        let service = self.service(config)?;
        if let Some(mut staged) = state.staged_restore.clone() {
            if staged.archive_path != request.archive_path {
                return Err(BackupControlError::StageAlreadyExists);
            }
            let evidence = state
                .staged_evidence
                .as_ref()
                .ok_or(BackupControlError::StageNeedsReview)?;
            if let Err(error) = service.reverify_staged(
                evidence,
                Path::new(&staged.archive_path),
                Path::new(&staged.staging_path),
            ) {
                staged.reason = format!(
                    "Staged verification failed: {error}. Owner review is required before retry."
                );
                state.staged_restore = Some(staged);
                state.last_error = Some(error.to_string());
                self.save_state(&state)?;
                return Err(error.into());
            }
            staged.verified_at_ms = crate::now_ms();
            staged.reason = "Staged data passed fresh archive and index readback. Activation remains unavailable pending engine-owned restore gates.".into();
            state.staged_restore = Some(staged.clone());
            state.last_error = None;
            self.save_state(&state)?;
            return Ok(staged);
        }
        let staged = service.stage_restore(Path::new(&request.archive_path))?;
        let mut status = BackupStageStatus {
            archive_path: request.archive_path,
            staging_path: staged.path.display().to_string(),
            verified_at_ms: 0,
            database_count: staged.preview.manifest.databases.len(),
            attachment_count: staged.preview.manifest.entries.iter().filter(|e| e.category == "attachment").count(),
            restore_activation_available: false,
            reason: "Staged data passed archive and index readback. Reverify after restart; activation remains unavailable pending engine-owned restore gates.".into(),
        };
        let evidence = match staged.evidence(Path::new(&status.archive_path)) {
            Ok(evidence) => evidence,
            Err(error) => {
                status.reason = format!(
                    "Staged verification failed: {error}. Owner review is required before retry."
                );
                state.staged_restore = Some(status);
                state.last_error = Some(error.to_string());
                self.save_state(&state)?;
                return Err(error.into());
            }
        };
        if let Err(error) = service.reverify_staged(
            &evidence,
            Path::new(&status.archive_path),
            Path::new(&status.staging_path),
        ) {
            status.reason = format!(
                "Staged verification failed: {error}. Owner review is required before retry."
            );
            state.staged_restore = Some(status);
            state.staged_evidence = Some(evidence);
            state.last_error = Some(error.to_string());
            self.save_state(&state)?;
            return Err(error.into());
        }
        status.verified_at_ms = crate::now_ms();
        state.staged_restore = Some(status.clone());
        state.staged_evidence = Some(evidence);
        state.last_error = None;
        self.save_state(&state)?;
        Ok(status)
    }

    /// Called by the engine's bounded timer. A missed day is retried at the
    /// next tick. Only a configured, separate copy-isolated source is read.
    pub fn tick_automatic(&self, now_ms: i64) -> Result<Option<BackupSummary>> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.load_state()?;
        let Some(config) = state.config.as_ref() else {
            return Ok(None);
        };
        if !config.automatic_enabled {
            return Ok(None);
        }
        let service = self.service(config)?;
        let receipt =
            service.backup_if_due(&config.destination_dir, now_ms, state.receipts.last())?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        let summary = summary(&receipt);
        state.receipts.push(receipt);
        state.last_error = None;
        self.save_state(&state)?;
        Ok(Some(summary))
    }

    pub fn record_scheduler_error(&self, message: String) -> Result<()> {
        let _guard = self
            .inner
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut state = self.load_state()?;
        state.last_error = Some(message);
        self.save_state(&state)
    }

    fn service(&self, config: &BackupConfig) -> Result<BackupService> {
        Ok(BackupService::new(
            self.service_config_is_valid(config)?,
            self.recovery_key()?,
        ))
    }

    fn service_config_is_valid(&self, config: &BackupConfig) -> Result<IsolatedRoot> {
        validate_isolated_destination(&config.destination_dir)?;
        let root = IsolatedRoot::open(&config.source_root)?;
        let owner = self.inner.owner_root.path();
        if root.path() == owner
            || root.path().starts_with(owner)
            || owner.starts_with(root.path())
            || config.destination_dir.starts_with(root.path())
            || root.path().starts_with(&config.destination_dir)
            || config.destination_dir.starts_with(owner)
            || owner.starts_with(&config.destination_dir)
        {
            return Err(BackupControlError::OverlappingRoots);
        }
        Ok(root)
    }

    fn recovery_key(&self) -> Result<RecoveryKey> {
        let state = self.load_state()?;
        if let Some(path) = state.recovery_key_file.as_deref() {
            return self.read_owner_key_file(path, state.config.as_ref());
        }
        // The legacy broker path remains solely for explicit in-memory
        // fixtures. A production status/read never probes macOS Keychain.
        if !self.inner.broker.is_in_memory_for_tests() {
            return Err(BackupControlError::KeyFileRequired);
        }
        let scope = CredentialScope::luna_backup();
        let metadata = self
            .inner
            .broker
            .list_metadata()?
            .into_iter()
            .find(|item| {
                item.scope == scope
                    && item.name == KEY_NAME
                    && item.state == CredentialState::Configured
            })
            .ok_or(BackupControlError::Unconfigured)?;
        let lease = self.inner.broker.lease_by_id(&metadata.id, &scope)?;
        let material = self.inner.broker.materialize(&lease, &scope)?;
        let mut bytes: [u8; 32] = material
            .as_bytes()
            .try_into()
            .map_err(|_| BackupControlError::InvalidRecoveryKey)?;
        let key = RecoveryKey::from_authorized_bytes(bytes);
        bytes.zeroize();
        Ok(key)
    }

    #[cfg(unix)]
    fn read_owner_key_file(
        &self,
        path: &Path,
        config: Option<&BackupConfig>,
    ) -> Result<RecoveryKey> {
        if !path.is_absolute() || key_file_is_in_provider_home(path) {
            return Err(BackupControlError::InvalidKeyFile);
        }
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(BackupControlError::InvalidKeyFile);
        }
        let canonical = path.canonicalize()?;
        let owner = self.inner.owner_root.path();
        if canonical.starts_with(owner)
            || key_file_is_in_provider_home(&canonical)
            || config.is_some_and(|config| {
                canonical.starts_with(&config.source_root)
                    || canonical.starts_with(&config.destination_dir)
            })
        {
            return Err(BackupControlError::OverlappingRoots);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(&canonical)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() || metadata.len() != 32 {
            return Err(BackupControlError::InvalidKeyFile);
        }
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o777 != 0o600
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(BackupControlError::InvalidKeyFile);
        }
        let mut bytes = Zeroizing::new([0u8; 32]);
        file.read_exact(&mut *bytes)?;
        Ok(RecoveryKey::from_authorized_bytes(*bytes))
    }

    #[cfg(not(unix))]
    fn read_owner_key_file(
        &self,
        _path: &Path,
        _config: Option<&BackupConfig>,
    ) -> Result<RecoveryKey> {
        Err(BackupControlError::InvalidKeyFile)
    }

    fn status_from_state(&self, state: ControlState) -> Result<BackupStatus> {
        let Some(config) = state.config else {
            return Ok(BackupStatus {
                availability: BackupAvailability::Unconfigured,
                reason: Some(
                    "Choose a separate copy-isolated LUNA source and marked backup destination."
                        .into(),
                ),
                source_root: None,
                destination_dir: None,
                automatic_enabled: false,
                key_configured: self.recovery_key().is_ok(),
                last_success: None,
                retention_candidates: 0,
                staged_restore: None,
                restore_activation_available: false,
                restore_reason: RESTORE_REASON.into(),
            });
        };
        let configuration = self
            .service_config_is_valid(&config)
            .map_err(|error| error.to_string());
        let key_state = self.recovery_key();
        let key_configured = key_state.is_ok();
        let mut reason = state.last_error;
        let mut verified_last = false;
        let availability = if let Err(error) = configuration {
            reason = Some(format!(
                "Copy-isolated backup configuration is unavailable: {error}"
            ));
            BackupAvailability::Error
        } else {
            match key_state {
                Ok(key) => {
                    if let Some(last) = state.receipts.last() {
                        match (
                            crate::backup::inspect_archive(&last.archive, &key),
                            file_sha256(&last.archive),
                        ) {
                            (Ok(manifest), Ok(digest))
                                if manifest == last.manifest && digest == last.archive_sha256 =>
                            {
                                verified_last = true;
                                BackupAvailability::Ready
                            }
                            _ => {
                                reason = Some(
                                    "Latest recorded backup failed fresh verification.".into(),
                                );
                                BackupAvailability::Error
                            }
                        }
                    } else {
                        BackupAvailability::Ready
                    }
                }
                Err(error) => {
                    reason = Some(format!("Backup recovery key unavailable: {error}"));
                    BackupAvailability::Unconfigured
                }
            }
        };
        Ok(BackupStatus {
            availability,
            reason,
            source_root: Some(config.source_root.display().to_string()),
            destination_dir: Some(config.destination_dir.display().to_string()),
            automatic_enabled: config.automatic_enabled,
            key_configured,
            last_success: verified_last.then(|| summary(state.receipts.last().unwrap())),
            retention_candidates: retention_candidates(&state.receipts).len(),
            staged_restore: state.staged_restore.map(|mut stage| {
                if state.staged_evidence.is_none()
                    && !stage.reason.starts_with("Staged verification failed:")
                {
                    stage.reason =
                        "Staged evidence predates restart-safe readback; owner review is required."
                            .into();
                } else if !Path::new(&stage.staging_path).is_dir() {
                    stage.reason =
                        "Staged directory is missing; owner review is required before retry."
                            .into();
                }
                stage
            }),
            restore_activation_available: false,
            restore_reason: RESTORE_REASON.into(),
        })
    }

    fn state_path(&self) -> PathBuf {
        self.inner.owner_root.path().join(STATE_FILE)
    }

    fn load_state(&self) -> Result<ControlState> {
        let path = self.state_path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Err(BackupControlError::IncompatibleState),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ControlState {
                    version: 1,
                    ..Default::default()
                });
            }
            Err(error) => return Err(error.into()),
        }
        let bytes = fs::read(&path)?;
        let state: ControlState = serde_json::from_slice(&bytes)?;
        if state.version != 1 {
            return Err(BackupControlError::IncompatibleState);
        }
        Ok(state)
    }

    fn save_state(&self, state: &ControlState) -> Result<()> {
        let root = self.inner.owner_root.path();
        let mut temp = NamedTempFile::new_in(root)?;
        serde_json::to_writer(temp.as_file_mut(), state)?;
        temp.as_file_mut().flush()?;
        temp.as_file_mut().sync_all()?;
        temp.persist(self.state_path())
            .map_err(|e| BackupControlError::Io(e.error))?;
        #[cfg(unix)]
        File::open(root)?.sync_all()?;
        Ok(())
    }
}

/// A backup recovery file must not alias a provider-owned credential home.
/// This only compares paths; it never opens or probes the provider stores.
fn key_file_is_in_provider_home(path: &Path) -> bool {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        homes.extend([
            home.join(".codex"),
            home.join(".claude"),
            home.join(".config/opencode"),
            home.join(".local/share/opencode"),
        ]);
    }
    for variable in [
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "OPENCODE_HOME",
        "OPENCODE_CONFIG_DIR",
    ] {
        if let Some(home) = std::env::var_os(variable).map(PathBuf::from)
            && home.is_absolute()
        {
            homes.push(home);
        }
    }
    homes.into_iter().any(|home| path.starts_with(home))
}

fn parse_key(hex: &str) -> Result<[u8; 32]> {
    if hex.len() != 64 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(BackupControlError::InvalidRecoveryKey);
    }
    let mut out = [0u8; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).map_err(|_| BackupControlError::InvalidRecoveryKey)?;
        out[index] =
            u8::from_str_radix(pair, 16).map_err(|_| BackupControlError::InvalidRecoveryKey)?;
    }
    Ok(out)
}

fn summary(receipt: &BackupReceipt) -> BackupSummary {
    BackupSummary {
        archive_path: receipt.archive.display().to_string(),
        archive_sha256: receipt.archive_sha256.clone(),
        created_at_ms: receipt.manifest.created_at_ms,
        total_bytes: receipt.manifest.total_bytes,
        entry_count: receipt.manifest.entries.len(),
    }
}

fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut HashWriter(&mut hash))?;
    Ok(format!("{:x}", hash.finalize()))
}

struct HashWriter<'a>(&'a mut sha2::Sha256);
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
