//! Engine-owned L17 control for explicit copy-isolated backup roots. This is
//! deliberately separate from the live engine profile until an offline
//! shutdown/startup handoff can own the real root.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use zeron_proto::{
    BackupArchiveRequest, BackupAvailability, BackupConfigureRequest, BackupPreview,
    BackupStageStatus, BackupStatus, BackupSummary,
};

use crate::backup::{
    BackupError, BackupReceipt, BackupService, IsolatedRoot, StagedEvidence, retention_candidates,
    validate_isolated_destination,
};

const STATE_FILE: &str = "backup-control-v1.json";
const RESTORE_REASON: &str = "Restore activation remains unavailable: chat2 remote frontier, provider mappings, usage reconstruction, and post-start engine readback are unresolved.";

#[derive(Debug, thiserror::Error)]
pub enum BackupControlError {
    #[error("backup: {0}")]
    Backup(#[from] BackupError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("backup configuration is incomplete")]
    Unconfigured,
    #[error("BB-style archives are unencrypted and do not use a recovery key")]
    KeyNotUsed,
    #[error("backup source, destination, and engine roots must be separate")]
    OverlappingRoots,
    #[error("backup control state is incompatible")]
    IncompatibleState,
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
    receipts: Vec<BackupReceipt>,
    #[serde(default)]
    staged_restore: Option<BackupStageStatus>,
    #[serde(default)]
    staged_evidence: Option<StagedEvidence>,
    last_error: Option<String>,
}

impl BackupControl {
    /// Construct only from an explicitly marked synthetic/copy-isolated engine
    /// root. This control never reads a credential broker or Keychain.
    pub fn new(owner_root: IsolatedRoot) -> Self {
        Self {
            inner: Arc::new(ControlInner {
                owner_root,
                operation: Mutex::new(()),
            }),
        }
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
        Ok(BackupService::new_bb_style(
            self.service_config_is_valid(config)?,
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
                key_configured: false,
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
        let mut reason = state.last_error;
        let mut verified_last = false;
        let availability = if let Err(error) = configuration {
            reason = Some(format!(
                "Copy-isolated backup configuration is unavailable: {error}"
            ));
            BackupAvailability::Error
        } else {
            if let Some(last) = state.receipts.last() {
                match (
                    self.service(&config)
                        .and_then(|service| service.inspect(&last.archive).map_err(Into::into)),
                    file_sha256(&last.archive),
                ) {
                    (Ok(manifest), Ok(digest))
                        if manifest == last.manifest && digest == last.archive_sha256 =>
                    {
                        verified_last = true;
                        BackupAvailability::Ready
                    }
                    _ => {
                        reason = Some("Latest recorded backup failed fresh verification.".into());
                        BackupAvailability::Error
                    }
                }
            } else {
                BackupAvailability::Ready
            }
        };
        Ok(BackupStatus {
            availability,
            reason,
            source_root: Some(config.source_root.display().to_string()),
            destination_dir: Some(config.destination_dir.display().to_string()),
            automatic_enabled: config.automatic_enabled,
            key_configured: false,
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
