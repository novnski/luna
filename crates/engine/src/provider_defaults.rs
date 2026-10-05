//! Atomic, profile-scoped defaults shared by desktop and agent chat creation.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use luna_proto::{ProviderDefaultsSnapshot, SetProviderDefaultsRequest};

use crate::{EngineError, device_write::DeviceWriteHandle};

const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
pub(crate) struct ProviderDefaults(Arc<Inner>);

struct Inner {
    root: PathBuf,
    writes: DeviceWriteHandle,
    gate: Mutex<()>,
}

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

impl ProviderDefaults {
    pub(crate) fn open(root: &Path, writes: DeviceWriteHandle) -> Result<Self, EngineError> {
        Ok(Self(Arc::new(Inner {
            root: root.canonicalize()?,
            writes,
            gate: Mutex::new(()),
        })))
    }

    fn path(&self) -> Result<PathBuf, EngineError> {
        if self.0.root.canonicalize()? != self.0.root {
            return Err(error("Provider defaults profile changed; reload settings"));
        }
        let path = self.0.root.join("provider-defaults.json");
        match std::fs::symlink_metadata(&path) {
            Ok(meta)
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_BYTES =>
            {
                return Err(error(
                    "Provider defaults must be a regular file smaller than 64 KiB",
                ));
            }
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
            _ => {}
        }
        Ok(path)
    }

    fn read(&self) -> Result<ProviderDefaultsSnapshot, EngineError> {
        match std::fs::read(self.path()?) {
            Ok(bytes) if bytes.len() as u64 <= MAX_BYTES => {
                serde_json::from_slice(&bytes).map_err(|err| {
                    error(format!(
                        "Provider defaults could not be read; the file was preserved: {err}"
                    ))
                })
            }
            Ok(_) => Err(error("Provider defaults exceed their storage limit")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(ProviderDefaultsSnapshot::default())
            }
            Err(err) => Err(err.into()),
        }
    }

    pub(crate) fn get(&self) -> Result<ProviderDefaultsSnapshot, EngineError> {
        let _gate = self
            .0
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.read()
    }

    /// Callers validate the exact selection against the account's model catalog
    /// before entering this synchronous, revision-checked write.
    pub(crate) fn set(
        &self,
        request: SetProviderDefaultsRequest,
    ) -> Result<ProviderDefaultsSnapshot, EngineError> {
        let _permit = self
            .0
            .writes
            .try_admit()
            .map_err(|err| error(format!("Provider settings unavailable: {err:?}")))?;
        let _gate = self
            .0
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = self.read()?;
        if next.revision != request.expected_revision {
            return Err(error("Provider defaults changed; reload before saving"));
        }
        match request.defaults {
            Some(defaults) => {
                next.providers.insert(request.provider, defaults);
            }
            None => {
                next.providers.remove(&request.provider);
            }
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| error("Provider defaults revision exhausted"))?;
        let bytes = serde_json::to_vec_pretty(&next)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(error("Provider defaults exceed their storage limit"));
        }
        let path = self.path()?;
        let temporary = self
            .0
            .root
            .join(format!(".provider-defaults-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), EngineError> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            #[cfg(unix)]
            if let Err(error) =
                std::fs::File::open(&self.0.root).and_then(|directory| directory.sync_all())
            {
                tracing::warn!(%error, "provider defaults committed; directory durability sync unavailable");
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        Ok(next)
    }
}

pub(crate) fn validate_selection(
    selection: &luna_proto::ProviderExecutionDefault,
    models: &[luna_proto::Model],
) -> Result<(), String> {
    let model = match selection.model.as_deref() {
        Some(id) => models.iter().find(|model| model.id == id),
        None => models.first(),
    }
    .ok_or_else(|| {
        "The selected default model is unavailable; refresh the model catalog".to_string()
    })?;
    if selection
        .reasoning
        .is_some_and(|level| !model.reasoning_levels.contains(&level))
    {
        return Err("The selected reasoning level is unavailable for this model".into());
    }
    for (key, value) in &selection.model_options {
        if !model.options.iter().any(|option| {
            option.id == *key
                && option
                    .choices
                    .iter()
                    .any(|choice| value.as_str() == Some(choice.id.as_str()))
        }) {
            return Err(format!(
                "Unsupported default model option {key:?}; choose a value from the model catalog"
            ));
        }
    }
    Ok(())
}
