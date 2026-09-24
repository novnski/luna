//! HarnessRegistry — the engine's harness catalog: eager instances (mock) plus lazy
//! CLI slots. Lazy slots carry a static descriptor so `ListHarnesses` never
//! forces a subprocess spawn.
//!
//! Also owns the device's harness ENABLEMENT (Settings → Providers): which harnesses
//! this device's composer offers, persisted in `{data_dir}/harness-prefs.json`.
//! Per-device because CLI installs are — a viewer retargets the settings page at
//! another device and edits THAT device's set over the forwarded RPCs.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use luna_harness::{Harness, HarnessError, mock::MockHarness};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, ReasoningLevel, SteeringMode};

use crate::device_write::{DeviceWriteHandle, DeviceWritePermit};

/// What `ListHarnesses` reports per harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessDescriptor {
    pub id: HarnessId,
    pub name: String,
    pub supports_steering: bool,
    pub steering_mode: SteeringMode,
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// Whether the agent's CLI is present on the listing device (the settings
    /// enable-gate). Defaults true so catalogs from engines predating the
    /// field never read as uninstallable.
    #[serde(default = "default_installed")]
    pub installed: bool,
    /// Whether the listing device offers this harness (Settings → Agents).
    /// `None` — the catalog came from an engine predating the setting — means
    /// "unknown": consumers fall back to detection (see [`descriptor_enabled`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl HarnessDescriptor {
    /// Whether this harness can accept a prompt inside the turn that is
    /// currently running. Turn-boundary steering is still useful to the
    /// automatic queue drain, but it is not the non-interrupting "Steer"
    /// action exposed on an individual queued row.
    pub fn steers_mid_turn(&self) -> bool {
        self.supports_steering && self.steering_mode == SteeringMode::StepBoundary
    }
}

fn default_installed() -> bool {
    true
}

/// Whether detection alone may switch a harness on. The mock always "resolves"
/// but is a test rig, and counting it would let the last real agent be turned
/// off (the composer needs something it can actually run); the dev rig opts it
/// in through `COMET_HARNESS=mock`, which bypasses enablement in the pickers.
fn auto_enabled(id: HarnessId) -> bool {
    id != HarnessId::Mock
}

/// A descriptor's effective enabled flag. `None` — a catalog from an engine
/// predating the setting — falls back to detection, the same rule new devices
/// start from (see [`HarnessRegistry::enabled_set`]).
pub fn descriptor_enabled(descriptor: &HarnessDescriptor) -> bool {
    descriptor
        .enabled
        .unwrap_or_else(|| descriptor.installed && auto_enabled(descriptor.id))
}

fn describe(harness: &dyn Harness) -> HarnessDescriptor {
    HarnessDescriptor {
        id: harness.id(),
        name: harness.display_name().to_string(),
        supports_steering: harness.supports_steering(),
        steering_mode: harness.steering_mode(),
        reasoning_levels: harness.reasoning_levels().to_vec(),
        installed: harness.installed(),
        enabled: None,
    }
}

/// The persisted shape of `harness-prefs.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HarnessPrefsFile {
    /// The user's explicit opt-OUTS. Enablement otherwise follows detection,
    /// so the file only records "no" — an agent installed later turns itself
    /// on without a trip to Settings.
    disabled: Vec<HarnessId>,
    titles: TitleSettings,
    /// The allow-list written back when enablement was a fixed default set.
    /// Read once, folded into `disabled`, and never written again.
    #[serde(skip_serializing)]
    enabled: Option<Vec<HarnessId>>,
}

/// Per-device automatic session title preferences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleSettings {
    /// None follows the session harness, using a supported installed fallback.
    pub harness: Option<HarnessId>,
    /// None selects the cheapest model offered by the selected harness.
    pub model: Option<String>,
}

type Factory = Box<dyn Fn() -> Result<Arc<dyn Harness>, HarnessError> + Send + Sync>;
type InstalledProbe = Box<dyn Fn() -> bool + Send + Sync>;

enum Slot {
    Ready(Arc<dyn Harness>),
    Lazy {
        descriptor: HarnessDescriptor,
        /// Re-run on every `descriptors()` call — a CLI installed mid-session
        /// shows up on the next settings/picker open, no restart needed.
        installed: InstalledProbe,
        factory: Factory,
    },
}

pub struct HarnessRegistry {
    slots: Mutex<HashMap<HarnessId, Slot>>,
    order: Mutex<Vec<HarnessId>>,
    /// This device's enabled set; `None` inner value = the default set.
    prefs: Mutex<HarnessPrefsFile>,
    /// Where the prefs persist; `None` (tests, bare registries) skips writes.
    prefs_path: Mutex<Option<PathBuf>>,
    /// Serialize candidate validation, publication, and in-memory commit.
    prefs_write: Mutex<()>,
    /// Weak engine-owned coordinator. Admission always precedes prefs_write.
    prefs_write_handle: Mutex<Option<DeviceWriteHandle>>,
    /// A rename with failed directory sync must be retried even if the next
    /// request repeats the already-visible setting.
    prefs_needs_sync: Mutex<bool>,
    #[cfg(test)]
    prefs_before_publish: Mutex<Option<Arc<dyn Fn() -> std::io::Result<()> + Send + Sync>>>,
    #[cfg(test)]
    prefs_after_publish: Mutex<Option<Arc<dyn Fn() -> std::io::Result<()> + Send + Sync>>>,
}

#[derive(Debug)]
pub(crate) enum PrefsMutationError {
    Failed(String),
    /// Rename succeeded, but syncing the parent directory failed. The new
    /// value is visible on disk and in memory; crash durability is uncertain.
    PublishedUncertain(String),
}

impl PrefsMutationError {
    pub(crate) fn published(&self) -> bool {
        matches!(self, Self::PublishedUncertain(_))
    }
}

impl std::fmt::Display for PrefsMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(message) | Self::PublishedUncertain(message) => f.write_str(message),
        }
    }
}

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            prefs: Mutex::new(HarnessPrefsFile::default()),
            prefs_path: Mutex::new(None),
            prefs_write: Mutex::new(()),
            prefs_write_handle: Mutex::new(None),
            prefs_needs_sync: Mutex::new(false),
            #[cfg(test)]
            prefs_before_publish: Mutex::new(None),
            #[cfg(test)]
            prefs_after_publish: Mutex::new(None),
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<HarnessId, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn order(&self) -> MutexGuard<'_, Vec<HarnessId>> {
        self.order.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn prefs(&self) -> MutexGuard<'_, HarnessPrefsFile> {
        self.prefs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn set_prefs_before_publish_for_tests(
        &self,
        hook: Option<Arc<dyn Fn() -> std::io::Result<()> + Send + Sync>>,
    ) {
        *self
            .prefs_before_publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = hook;
    }

    /// Load `harness-prefs.json` from the engine data dir and remember the
    /// path for writes. Corrupt/missing files fall back to the default set.
    pub fn load_prefs(&self, data_dir: &Path) {
        if let Err(error) = self.load_prefs_checked(data_dir) {
            tracing::warn!(%error, "harness-prefs load or migration failed");
        }
    }

    pub(crate) fn load_prefs_guarded(
        &self,
        data_dir: &Path,
        write_handle: DeviceWriteHandle,
    ) -> Result<(), PrefsMutationError> {
        let _permit = write_handle.try_admit().map_err(|error| {
            PrefsMutationError::Failed(format!(
                "harness preferences temporarily unavailable ({error:?}); retry"
            ))
        })?;
        let root = data_dir
            .canonicalize()
            .map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
        if root != _permit.ticket().root() {
            return Err(PrefsMutationError::Failed(
                "harness preferences are outside the admitted device root".into(),
            ));
        }
        let _write = self
            .prefs_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Bind under the mutation lock, before a legacy load can migrate.
        // A writer admitted by a previous owner will fail the identity check
        // when it reaches this lock rather than mutating under the new owner.
        *self
            .prefs_write_handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(write_handle);
        self.load_prefs_under_write(&root)
    }

    fn load_prefs_checked(&self, data_dir: &Path) -> Result<(), PrefsMutationError> {
        let _permit = self.admit_prefs_write()?;
        let guarded_root = if let Some(permit) = _permit.as_ref() {
            let root = data_dir
                .canonicalize()
                .map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
            if root != permit.ticket().root() {
                return Err(PrefsMutationError::Failed(
                    "harness preferences are outside the admitted device root".into(),
                ));
            }
            Some(root)
        } else {
            None
        };
        let _write = self
            .prefs_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.check_prefs_permit(_permit.as_ref())?;
        self.load_prefs_under_write(guarded_root.as_deref().unwrap_or(data_dir))
    }

    fn load_prefs_under_write(&self, data_dir: &Path) -> Result<(), PrefsMutationError> {
        let path = data_dir.join("harness-prefs.json");
        let mut loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HarnessPrefsFile>(&text).ok())
            .unwrap_or_default();
        let had_legacy = loaded.enabled.is_some();
        let had_retired = loaded.disabled.iter().any(|id| id.is_retired())
            || loaded.titles.harness.is_some_and(HarnessId::is_retired);
        loaded.disabled.retain(|id| !id.is_retired());
        if loaded.titles.harness.is_some_and(HarnessId::is_retired) {
            loaded.titles = TitleSettings::default();
        }
        let needs_sync = *self
            .prefs_needs_sync
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let result = if let Some(legacy) = loaded.enabled.take() {
            // Fold the legacy allow-list into explicit opt-outs, but publish
            // before exposing the migrated in-memory state.
            loaded.disabled = self
                .order()
                .iter()
                .copied()
                .filter(|id| auto_enabled(*id) && !legacy.contains(id))
                .collect();
            self.publish_prefs(&path, &loaded)
        } else if had_retired || needs_sync {
            self.publish_prefs(&path, &loaded)
        } else {
            Ok(())
        };
        if result
            .as_ref()
            .map_or_else(PrefsMutationError::published, |_| true)
        {
            *self.prefs() = loaded;
            *self
                .prefs_path
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(path);
        }
        if (had_legacy || had_retired || needs_sync) && result.is_ok() {
            *self
                .prefs_needs_sync
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = false;
        } else if (had_legacy || had_retired || needs_sync)
            && result.as_ref().is_err_and(|error| error.published())
        {
            *self
                .prefs_needs_sync
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = true;
        }
        result
    }

    /// What this device offers: every harness whose CLI is FOUND, minus the
    /// user's explicit opt-outs. Enablement follows detection, so installing
    /// an agent is all it takes for it to appear in the composer.
    pub fn enabled_set(&self) -> Vec<HarnessId> {
        // Both guards drop before the installed probes run: `descriptors()`
        // takes `slots` then `order`, so holding `order` across a probe (which
        // takes `slots`) would invert the lock order.
        let registered: Vec<HarnessId> = self.order().iter().copied().collect();
        let disabled = self.prefs().disabled.clone();
        registered
            .into_iter()
            .filter(|id| auto_enabled(*id) && !disabled.contains(id) && self.installed_for(*id))
            .collect()
    }

    /// Whether this device's CLI probe passes for `id` (no spawn, no resolve).
    fn installed_for(&self, id: HarnessId) -> bool {
        match self.slots().get(&id) {
            Some(Slot::Ready(harness)) => harness.installed(),
            Some(Slot::Lazy { installed, .. }) => installed(),
            None => false,
        }
    }

    /// Flip one harness's enablement and persist. Refuses unknown harnesses,
    /// enabling one whose CLI is missing (the settings gate, enforced where
    /// the state lives). With all providers off, the composer shows its
    /// existing no-provider state until one is enabled again.
    pub fn set_enabled(&self, id: HarnessId, on: bool) -> Result<(), String> {
        let permit = self
            .admit_prefs_write()
            .map_err(|error| error.to_string())?;
        self.set_enabled_with_permit(id, on, permit.as_ref())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn admit_prefs_write(
        &self,
    ) -> Result<Option<DeviceWritePermit>, PrefsMutationError> {
        self.prefs_write_handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(|handle| {
                handle.try_admit().map_err(|error| {
                    PrefsMutationError::Failed(format!(
                        "harness preferences temporarily unavailable ({error:?}); retry"
                    ))
                })
            })
            .transpose()
    }

    fn check_prefs_permit(
        &self,
        permit: Option<&DeviceWritePermit>,
    ) -> Result<(), PrefsMutationError> {
        let handle = self
            .prefs_write_handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if handle.is_some_and(|handle| !permit.is_some_and(|permit| handle.owns(permit))) {
            return Err(PrefsMutationError::Failed(
                "harness preferences require the current device writer; retry".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn set_enabled_with_permit(
        &self,
        id: HarnessId,
        on: bool,
        permit: Option<&DeviceWritePermit>,
    ) -> Result<(), PrefsMutationError> {
        self.check_prefs_permit(permit)?;
        let _write = self
            .prefs_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.check_prefs_permit(permit)?;
        if !self.slots().contains_key(&id) {
            return Err(PrefsMutationError::Failed(format!(
                "unknown harness {id:?}"
            )));
        }
        if on && !auto_enabled(id) {
            return Err(PrefsMutationError::Failed(format!(
                "{id:?} cannot be enabled from Settings"
            )));
        }
        if on && !self.installed_for(id) {
            return Err(PrefsMutationError::Failed(format!(
                "{id:?} CLI is not installed on this device"
            )));
        }
        let enabled = self.enabled_set();
        let mut candidate = self.prefs().clone();
        match (on, enabled.contains(&id)) {
            (true, false) => {
                candidate.disabled.retain(|h| *h != id);
            }
            (false, true) => {
                candidate.disabled.push(id);
            }
            _ => {
                let needs_sync = *self
                    .prefs_needs_sync
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                return if needs_sync {
                    self.commit_prefs(candidate)
                } else {
                    Ok(())
                };
            }
        }
        self.commit_prefs(candidate)
    }

    fn commit_prefs(&self, candidate: HarnessPrefsFile) -> Result<(), PrefsMutationError> {
        let Some(path) = self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            *self.prefs() = candidate;
            return Ok(());
        };
        let result = self.publish_prefs(&path, &candidate);
        if result
            .as_ref()
            .map_or_else(PrefsMutationError::published, |_| true)
        {
            *self.prefs() = candidate;
        }
        if result.is_ok() {
            *self
                .prefs_needs_sync
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = false;
        } else if result.as_ref().is_err_and(|error| error.published()) {
            *self
                .prefs_needs_sync
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = true;
        }
        result
    }

    fn publish_prefs(
        &self,
        path: &Path,
        candidate: &HarnessPrefsFile,
    ) -> Result<(), PrefsMutationError> {
        let bytes = serde_json::to_vec_pretty(candidate)
            .map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
        let parent = path.parent().ok_or_else(|| {
            PrefsMutationError::Failed("harness preferences have no parent directory".into())
        })?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".harness-prefs-")
            .suffix(".tmp")
            .tempfile_in(parent)
            .map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.flush())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
        #[cfg(test)]
        if let Some(hook) = self
            .prefs_before_publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            hook().map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
        }
        temporary
            .into_temp_path()
            .persist(path)
            .map_err(|error| PrefsMutationError::Failed(error.to_string()))?;
        #[cfg(test)]
        if let Some(hook) = self
            .prefs_after_publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            hook().map_err(|error| {
                PrefsMutationError::PublishedUncertain(format!(
                    "harness preferences were published but durability is uncertain ({error}); refresh before retrying"
                ))
            })?;
        }
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                PrefsMutationError::PublishedUncertain(format!(
                    "harness preferences were published but directory sync failed ({error}); refresh before retrying"
                ))
            })?;
        Ok(())
    }

    pub fn title_settings(&self) -> TitleSettings {
        self.prefs().titles.clone()
    }

    pub fn set_title_settings(&self, mut settings: TitleSettings) -> Result<(), String> {
        let permit = self
            .admit_prefs_write()
            .map_err(|error| error.to_string())?;
        self.check_prefs_permit(permit.as_ref())
            .map_err(|error| error.to_string())?;
        let _write = self
            .prefs_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.check_prefs_permit(permit.as_ref())
            .map_err(|error| error.to_string())?;
        if let Some(id) = settings.harness {
            if !luna_harness::supports_titles(id) || !self.enabled_set().contains(&id) {
                return Err("Choose an enabled harness that supports title generation".into());
            }
        } else if settings.model.is_some() {
            return Err("Choose a title harness before choosing a model".into());
        }
        settings.model = settings.model.filter(|model| !model.trim().is_empty());
        let mut candidate = self.prefs().clone();
        candidate.titles = settings;
        self.commit_prefs(candidate)
            .map_err(|error| error.to_string())
    }

    pub fn register(&self, harness: Arc<dyn Harness>) {
        let id = harness.id();
        if self.slots().insert(id, Slot::Ready(harness)).is_none() {
            self.order().push(id);
        }
    }

    /// Register a slot resolved on first `resolve` (the factory result is
    /// cached). `installed` is the CLI-presence probe run per `descriptors()`
    /// call; it must never spawn.
    pub fn register_lazy(
        &self,
        descriptor: HarnessDescriptor,
        installed: InstalledProbe,
        factory: Factory,
    ) {
        let id = descriptor.id;
        if self
            .slots()
            .insert(
                id,
                Slot::Lazy {
                    descriptor,
                    installed,
                    factory,
                },
            )
            .is_none()
        {
            self.order().push(id);
        }
    }

    pub fn resolve(&self, id: HarnessId) -> Result<Arc<dyn Harness>, HarnessError> {
        let mut slots = self.slots();
        match slots.get(&id) {
            Some(Slot::Ready(harness)) => Ok(harness.clone()),
            Some(Slot::Lazy { factory, .. }) => {
                let harness = factory()?;
                slots.insert(id, Slot::Ready(harness.clone()));
                Ok(harness)
            }
            None => Err(HarnessError::NotInstalled(format!("{id:?}"))),
        }
    }

    /// Catalog for `ListHarnesses` — never forces a lazy resolve.
    pub fn descriptors(&self) -> Vec<HarnessDescriptor> {
        let enabled = self.enabled_set();
        let slots = self.slots();
        self.order()
            .iter()
            .filter_map(|id| {
                let mut descriptor = match slots.get(id) {
                    Some(Slot::Ready(harness)) => describe(harness.as_ref()),
                    Some(Slot::Lazy {
                        descriptor,
                        installed,
                        ..
                    }) => HarnessDescriptor {
                        installed: installed(),
                        ..descriptor.clone()
                    },
                    None => return None,
                };
                descriptor.enabled = Some(enabled.contains(id));
                Some(descriptor)
            })
            .collect()
    }
}

/// The production registry: MockHarness (hidden from production pickers) plus a lazy
/// `claude-code` slot resolved through `luna_harness` on first use (subprocess
/// discovery only happens when a run/model call actually needs it).
pub fn default_registry() -> HarnessRegistry {
    // Warm the login-shell PATH snapshot in the background so the first
    // claude/codex resolve doesn't pay the shell-startup latency inline.
    luna_harness::shell_env::prewarm();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "## Streaming pipeline\n\nEvery turn flows through the same path:\n\n".into(),
            },
            AgentEvent::TextDelta {
                text: "1. **Doc command** — the composer queues a durable `run` entry\n2. **Host executor** — the chat's host device marks it processed, then dispatches\n3. **Fold** — events fold into parts and diff into the Loro doc every 120ms\n\n".into(),
            },
            AgentEvent::ToolCall {
                id: "mock-tool-1".into(),
                call: luna_proto::ToolCall::Exec {
                    command: "cargo test --workspace".into(),
                },
            },
            AgentEvent::ToolResult {
                id: "mock-tool-1".into(),
                is_error: false,
                output: None,
                diff: None,
            },
            AgentEvent::ToolCall {
                id: "mock-tool-2".into(),
                call: luna_proto::ToolCall::Exec {
                    command: "git log -5 --oneline --decorate && git merge-base HEAD origin/main"
                        .into(),
                },
            },
            AgentEvent::ToolResult {
                id: "mock-tool-2".into(),
                is_error: false,
                output: None,
                diff: None,
            },
            AgentEvent::TextDelta {
                text: "The `SegmentWriter` appends into `LoroText` so the oplog stays RLE-merged:\n\n```rust\nfolded = fold_event_into_parts(&folded, &event);\nwriter.sync(&folded)?; // 120ms coalesced commits\n```\n\nSynced to every device through the session room. *Mock harness reporting in.*".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    }));
    registry.register_lazy(
        HarnessDescriptor {
            id: HarnessId::ClaudeCode,
            name: "Claude Code".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            // Must mirror ClaudeHarness exactly — the descriptor-stability
            // rule (see the codex test below).
            reasoning_levels: vec![
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
            ],
            installed: true,
            enabled: None,
        },
        Box::new(|| luna_harness::ClaudeHarness::new().installed()),
        Box::new(|| Ok(Arc::new(luna_harness::ClaudeHarness::new()) as Arc<dyn Harness>)),
    );
    // Codex, same lazy pattern: the static descriptor mirrors CodexHarness
    // exactly (`describe()` after the first resolve must not change the
    // catalog entry) — "Codex" per the original HARNESS_LABEL, StepBoundary
    // steering via native `turn/steer`, and the unified reasoning ladder from
    // luna_harness::codex::catalog. CLI discovery only happens when a
    // run/model call actually resolves the slot.
    registry.register_lazy(
        HarnessDescriptor {
            id: HarnessId::Codex,
            name: "Codex".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: vec![
                ReasoningLevel::Minimal,
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
                ReasoningLevel::Ultra,
            ],
            installed: true,
            enabled: None,
        },
        Box::new(|| luna_harness::CodexHarness::new().installed()),
        Box::new(|| Ok(Arc::new(luna_harness::CodexHarness::new()) as Arc<dyn Harness>)),
    );
    // opencode over its NATIVE HTTP/SSE protocol (the one the opencode
    // desktop app speaks — `opencode serve` + the /global/event bus), same
    // lazy pattern: the static descriptor mirrors OpencodeHarness exactly.
    // Turn-boundary steering; the effort ladder rides model VARIANTS (the
    // run sends the first advertised variant id for the picked level).
    registry.register_lazy(
        HarnessDescriptor {
            id: HarnessId::Opencode,
            name: "OpenCode".into(),
            supports_steering: true,
            steering_mode: SteeringMode::TurnBoundary,
            reasoning_levels: vec![
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
            ],
            installed: true,
            enabled: None,
        },
        Box::new(|| luna_harness::OpencodeHarness::new().installed()),
        Box::new(|| Ok(Arc::new(luna_harness::OpencodeHarness::new()) as Arc<dyn Harness>)),
    );
    registry
}

/// No shell prewarm or native provider slots: only the deterministic mock.
#[doc(hidden)]
pub fn local_adoption_mock_registry() -> HarnessRegistry {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "Synthetic LUNA fixture response.".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    }));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mid_turn_steering_requires_support_and_a_step_boundary() {
        let mut descriptor = HarnessDescriptor {
            id: HarnessId::Mock,
            name: "Mock".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: Vec::new(),
            installed: true,
            enabled: Some(true),
        };
        assert!(descriptor.steers_mid_turn());

        descriptor.steering_mode = SteeringMode::TurnBoundary;
        assert!(!descriptor.steers_mid_turn());

        descriptor.steering_mode = SteeringMode::StepBoundary;
        descriptor.supports_steering = false;
        assert!(!descriptor.steers_mid_turn());
    }

    #[test]
    fn lazy_slot_lists_without_resolving() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let registry = HarnessRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        registry.register_lazy(
            HarnessDescriptor {
                id: HarnessId::Mock,
                name: "Lazy Mock".into(),
                supports_steering: true,
                steering_mode: SteeringMode::StepBoundary,
                reasoning_levels: vec![],
                installed: true,
                enabled: None,
            },
            Box::new(|| false),
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Err(HarnessError::NotInstalled("nope".into()))
            }),
        );
        let listed = registry.descriptors();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Lazy Mock");
        // The listing runs the probe, not the stored placeholder.
        assert!(!listed[0].installed);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "listing must not force a resolve"
        );
        assert!(registry.resolve(HarnessId::Mock).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Catalogs serialized by engines that predate the `installed`/`enabled`
    /// fields must keep deserializing — installed, and enabled per the
    /// detection fallback.
    #[test]
    fn descriptor_without_new_fields_parses_with_fallbacks() {
        let parse = |id: &str| -> HarnessDescriptor {
            serde_json::from_str(&format!(
                r#"{{
                    "id": "{id}",
                    "name": "x",
                    "supportsSteering": true,
                    "steeringMode": "step-boundary",
                    "reasoningLevels": []
                }}"#
            ))
            .unwrap()
        };
        let claude = parse("claude-code");
        assert!(claude.installed);
        assert_eq!(claude.enabled, None);
        // Unknown enablement follows detection: a found CLI is offered...
        assert!(descriptor_enabled(&claude));
        // ...and one this device never found is not.
        let missing = HarnessDescriptor {
            installed: false,
            ..parse("grok")
        };
        assert!(!descriptor_enabled(&missing));
    }

    /// A registry slot for the tests below: installed probe fixed, factory
    /// never expected to run.
    fn test_slot(registry: &HarnessRegistry, id: HarnessId, installed: bool) {
        registry.register_lazy(
            HarnessDescriptor {
                id,
                name: format!("{id:?}"),
                supports_steering: true,
                steering_mode: SteeringMode::StepBoundary,
                reasoning_levels: vec![],
                installed: true,
                enabled: None,
            },
            Box::new(move || installed),
            Box::new(|| Err(HarnessError::NotInstalled("test slot".into()))),
        );
    }

    /// `descriptors()` stamps the per-device enabled flag; `set_enabled`
    /// guards the gate (no enabling missing CLIs, no disabling the last one)
    /// and persists across a reload.

    /// The point of following detection: an agent installed after the user has
    /// already edited Settings turns itself on, while the ones they switched
    /// off stay off. An allow-list can't express that — it can't tell "the
    /// user said no" from "this wasn't installed yet".
    #[test]
    fn newly_found_harnesses_enable_themselves_without_reviving_opt_outs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::ClaudeCode, true);
        test_slot(&registry, HarnessId::Codex, true);
        // Not installed yet — the probe flips when the user installs the CLI.
        let found = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&found);
        registry.register_lazy(
            HarnessDescriptor {
                id: HarnessId::Grok,
                name: "Grok".into(),
                supports_steering: true,
                steering_mode: SteeringMode::TurnBoundary,
                reasoning_levels: vec![],
                installed: true,
                enabled: None,
            },
            Box::new(move || probe.load(Ordering::SeqCst)),
            Box::new(|| Err(HarnessError::NotInstalled("test slot".into()))),
        );
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        assert_eq!(registry.enabled_set(), vec![HarnessId::ClaudeCode]);

        // The CLI appears mid-session; no restart, no visit to Settings.
        found.store(true, Ordering::SeqCst);
        assert_eq!(
            registry.enabled_set(),
            vec![HarnessId::ClaudeCode, HarnessId::Grok]
        );
        // The opt-out survives the reload that picks the new agent up.
        let reloaded = HarnessRegistry::new();
        reloaded.load_prefs(dir.path());
        test_slot(&reloaded, HarnessId::ClaudeCode, true);
        test_slot(&reloaded, HarnessId::Codex, true);
        test_slot(&reloaded, HarnessId::Grok, true);
        assert_eq!(
            reloaded.enabled_set(),
            vec![HarnessId::ClaudeCode, HarnessId::Grok]
        );
    }

    /// The mock resolves on every machine, so detection alone would enable it
    /// and let the last REAL agent be switched off — leaving a composer with
    /// nothing runnable behind a guard that thinks it is covered.
    #[test]
    fn detection_never_enables_the_mock() {
        let registry = default_registry();
        let enabled = registry.enabled_set();
        assert!(!enabled.contains(&HarnessId::Mock), "{enabled:?}");
        let mock = registry
            .descriptors()
            .into_iter()
            .find(|d| d.id == HarnessId::Mock)
            .expect("mock slot");
        assert!(mock.installed);
        assert_eq!(mock.enabled, Some(false));
    }

    /// A prefs file from the fixed-default era is an allow-list: everything
    /// registered and absent from it was a deliberate "no", so it converts to
    /// opt-outs rather than silently re-enabling on the next launch.
    #[test]
    fn legacy_allow_list_migrates_to_opt_outs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("harness-prefs.json"),
            r#"{ "enabled": ["claude-code"] }"#,
        )
        .unwrap();
        let registry = HarnessRegistry::new();
        test_slot(&registry, HarnessId::ClaudeCode, true);
        test_slot(&registry, HarnessId::Codex, true);
        test_slot(&registry, HarnessId::Grok, true);
        registry.load_prefs(dir.path());
        assert_eq!(registry.enabled_set(), vec![HarnessId::ClaudeCode]);

        // The rewritten file is the new shape, and the legacy key is gone.
        let text = std::fs::read_to_string(dir.path().join("harness-prefs.json")).unwrap();
        assert!(!text.contains("enabled"), "{text}");
        assert!(text.contains("codex") && text.contains("grok"), "{text}");
        // An agent registered after the migration is new, not a past "no".
        test_slot(&registry, HarnessId::Cursor, true);
        assert_eq!(
            registry.enabled_set(),
            vec![HarnessId::ClaudeCode, HarnessId::Cursor]
        );
    }

    /// The fresh-machine shape (#128): no CLIs installed at all. Under
    /// detection-based enablement nothing is enabled to begin with — no
    /// dimmed default toggles to dismiss — and switching an uninstalled
    /// harness "off" is a clean no-op, not an error.
    #[test]
    fn machine_without_clis_enables_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::ClaudeCode, false);
        test_slot(&registry, HarnessId::Codex, false);

        assert_eq!(registry.enabled_set(), Vec::<HarnessId>::new());
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        registry.set_enabled(HarnessId::ClaudeCode, false).unwrap();
        assert_eq!(registry.enabled_set(), Vec::<HarnessId>::new());

        let reloaded = HarnessRegistry::new();
        reloaded.load_prefs(dir.path());
        assert_eq!(reloaded.enabled_set(), Vec::<HarnessId>::new());
    }

    /// The Codex lazy descriptor must be indistinguishable from `describe()`
    /// after the first resolve — otherwise the catalog entry silently changes
    /// the moment the harness is used (name/ladder flip in the picker rail).
    /// (KNOWN GAP, predates this slot: the claude-code descriptor advertises
    /// `[Ultrathink]` while the resolved adapter reports `[Low..Max]` — left
    /// as-is here; flagged for its own pass.)
    #[test]
    fn codex_lazy_descriptor_matches_resolved_harness() {
        let registry = default_registry();
        let before = registry
            .descriptors()
            .into_iter()
            .find(|d| d.id == HarnessId::Codex)
            .unwrap();
        registry.resolve(HarnessId::Codex).unwrap();
        let after = registry
            .descriptors()
            .into_iter()
            .find(|d| d.id == HarnessId::Codex)
            .unwrap();
        assert_eq!(before.name, after.name);
        assert_eq!(before.supports_steering, after.supports_steering);
        assert_eq!(before.steering_mode, after.steering_mode);
        assert_eq!(before.reasoning_levels, after.reasoning_levels);
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn title_preferences_persist_and_validate_harness_model_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        registry.register(Arc::new(
            luna_harness::ClaudeHarness::new().with_executable(std::env::current_exe().unwrap()),
        ));
        let settings = TitleSettings {
            harness: Some(HarnessId::ClaudeCode),
            model: Some("haiku-test".into()),
        };
        registry.set_title_settings(settings.clone()).unwrap();
        let reloaded = HarnessRegistry::new();
        reloaded.load_prefs(dir.path());
        assert_eq!(reloaded.title_settings(), settings);
        assert!(
            registry
                .set_title_settings(TitleSettings {
                    harness: None,
                    model: Some("orphan".into())
                })
                .is_err()
        );
        assert!(
            registry
                .set_title_settings(TitleSettings {
                    harness: Some(HarnessId::Cursor),
                    model: None
                })
                .is_err()
        );
        assert_eq!(registry.title_settings(), settings);
        registry
            .set_title_settings(TitleSettings::default())
            .unwrap();
        reloaded.load_prefs(dir.path());
        assert_eq!(reloaded.title_settings(), TitleSettings::default());
    }

    #[test]
    fn old_harness_preferences_default_to_automatic_titles() {
        let prefs: HarnessPrefsFile = serde_json::from_str(r#"{"disabled":["codex"]}"#).unwrap();
        assert_eq!(prefs.titles, TitleSettings::default());
        assert_eq!(prefs.disabled, vec![HarnessId::Codex]);
    }
}

#[cfg(test)]
mod guarded_prefs_tests {
    use super::*;
    use crate::{InstanceLock, device_write::DeviceWriteOwner};
    use luna_sync::write_epoch::EpochError;
    use std::sync::mpsc;
    use std::time::Duration;

    fn slot(registry: &HarnessRegistry, id: HarnessId) {
        registry.register_lazy(
            HarnessDescriptor {
                id,
                name: format!("synthetic-{id:?}"),
                supports_steering: false,
                steering_mode: SteeringMode::TurnBoundary,
                reasoning_levels: vec![],
                installed: true,
                enabled: None,
            },
            Box::new(|| true),
            Box::new(|| Err(HarnessError::NotInstalled("synthetic slot".into()))),
        );
    }

    fn guarded(root: &tempfile::TempDir) -> (DeviceWriteOwner, Arc<HarnessRegistry>) {
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let registry = Arc::new(HarnessRegistry::new());
        slot(&registry, HarnessId::ClaudeCode);
        slot(&registry, HarnessId::Codex);
        registry
            .load_prefs_guarded(root.path(), owner.handle())
            .unwrap();
        (owner, registry)
    }

    #[tokio::test]
    async fn frozen_or_retired_registry_keeps_file_and_memory_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let (owner, registry) = guarded(&root);
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        let path = root.path().join("harness-prefs.json");
        let before = std::fs::read(&path).unwrap();
        let snapshot = owner
            .epoch_for_tests()
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        assert!(
            registry
                .set_enabled(HarnessId::Codex, true)
                .unwrap_err()
                .contains("Busy")
        );
        assert!(
            registry
                .set_title_settings(TitleSettings::default())
                .unwrap_err()
                .contains("Busy")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(registry.enabled_set(), vec![HarnessId::ClaudeCode]);
        drop(snapshot);
        registry.set_enabled(HarnessId::Codex, true).unwrap();
        let after = std::fs::read(&path).unwrap();
        owner.retire();
        assert!(
            registry
                .set_enabled(HarnessId::Codex, false)
                .unwrap_err()
                .contains("Retired")
        );
        assert_eq!(std::fs::read(&path).unwrap(), after);
        assert_eq!(registry.enabled_set().len(), 2);
        drop(owner);
        let replacement =
            DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
                .unwrap();
        let reloaded = HarnessRegistry::new();
        slot(&reloaded, HarnessId::ClaudeCode);
        slot(&reloaded, HarnessId::Codex);
        reloaded
            .load_prefs_guarded(root.path(), replacement.handle())
            .unwrap();
        assert_eq!(reloaded.enabled_set().len(), 2);
    }

    #[test]
    fn failed_publication_does_not_clobber_existing_prefs_or_state() {
        let root = tempfile::tempdir().unwrap();
        let (_owner, registry) = guarded(&root);
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        let path = root.path().join("harness-prefs.json");
        let before = std::fs::read(&path).unwrap();
        *registry.prefs_before_publish.lock().unwrap() = Some(Arc::new(|| {
            Err(std::io::Error::other("synthetic prepublication failure"))
        }));
        let error = registry.set_enabled(HarnessId::Codex, true).unwrap_err();
        assert!(error.contains("synthetic prepublication failure"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(registry.enabled_set(), vec![HarnessId::ClaudeCode]);
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".harness-prefs-")
                })
                .count(),
            0
        );
        *registry.prefs_before_publish.lock().unwrap() = None;
        registry.set_enabled(HarnessId::Codex, true).unwrap();
        assert_eq!(registry.enabled_set().len(), 2);
    }

    #[test]
    fn postpublication_sync_failure_reports_uncertainty_with_truthful_state() {
        let root = tempfile::tempdir().unwrap();
        let (_owner, registry) = guarded(&root);
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        *registry.prefs_after_publish.lock().unwrap() = Some(Arc::new(|| {
            Err(std::io::Error::other("synthetic directory sync failure"))
        }));
        let error = registry.set_enabled(HarnessId::Codex, true).unwrap_err();
        assert!(error.contains("published but durability is uncertain"));
        assert_eq!(registry.enabled_set().len(), 2);
        let reloaded = HarnessRegistry::new();
        slot(&reloaded, HarnessId::ClaudeCode);
        slot(&reloaded, HarnessId::Codex);
        reloaded.load_prefs(root.path());
        assert_eq!(reloaded.enabled_set(), registry.enabled_set());
        *registry.prefs_after_publish.lock().unwrap() = None;
        // A repeated no-op request must still retry the failed directory sync.
        registry.set_enabled(HarnessId::Codex, true).unwrap();
        assert!(!*registry.prefs_needs_sync.lock().unwrap());
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        assert_eq!(registry.enabled_set(), vec![HarnessId::ClaudeCode]);
    }

    #[tokio::test]
    async fn in_flight_publication_drains_before_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let (owner, registry) = guarded(&root);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        *registry.prefs_before_publish.lock().unwrap() = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(())
        }));
        let writer = registry.clone();
        let join = std::thread::spawn(move || writer.set_enabled(HarnessId::Codex, false));
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        let epoch = owner.epoch_for_tests();
        assert!(matches!(
            epoch.freeze(Duration::from_millis(40)).await,
            Err(EpochError::TimedOut)
        ));
        assert!(!root.path().join("harness-prefs.json").exists());
        release_tx.send(()).unwrap();
        tokio::task::spawn_blocking(move || join.join().unwrap().unwrap())
            .await
            .unwrap();
        let snapshot = epoch
            .freeze(Duration::from_secs(2))
            .await
            .unwrap()
            .into_snapshot()
            .unwrap();
        assert!(
            std::fs::read_to_string(root.path().join("harness-prefs.json"))
                .unwrap()
                .contains("codex")
        );
        drop(snapshot);
    }

    #[test]
    fn concurrent_title_and_toggle_updates_keep_both_changes() {
        let root = tempfile::tempdir().unwrap();
        let (_owner, registry) = guarded(&root);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let toggle = {
            let registry = registry.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry.set_enabled(HarnessId::Codex, false).unwrap();
            })
        };
        let title = {
            let registry = registry.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry
                    .set_title_settings(TitleSettings {
                        harness: Some(HarnessId::ClaudeCode),
                        model: Some("synthetic-model".into()),
                    })
                    .unwrap();
            })
        };
        barrier.wait();
        toggle.join().unwrap();
        title.join().unwrap();
        let reloaded = HarnessRegistry::new();
        slot(&reloaded, HarnessId::ClaudeCode);
        slot(&reloaded, HarnessId::Codex);
        reloaded.load_prefs(root.path());
        assert_eq!(reloaded.enabled_set(), vec![HarnessId::ClaudeCode]);
        assert_eq!(reloaded.title_settings(), registry.title_settings());
        assert_eq!(
            reloaded.title_settings().model.as_deref(),
            Some("synthetic-model")
        );
    }

    #[test]
    fn failed_legacy_migration_is_explicit_then_retries_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("harness-prefs.json");
        let legacy = br#"{"enabled":["claude-code"]}"#;
        std::fs::write(&path, legacy).unwrap();
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let registry = HarnessRegistry::new();
        slot(&registry, HarnessId::ClaudeCode);
        slot(&registry, HarnessId::Codex);
        *registry.prefs_before_publish.lock().unwrap() = Some(Arc::new(|| {
            Err(std::io::Error::other("synthetic migration failure"))
        }));
        let error = registry
            .load_prefs_guarded(root.path(), owner.handle())
            .unwrap_err();
        assert!(error.to_string().contains("synthetic migration failure"));
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        assert_eq!(registry.enabled_set().len(), 2);
        *registry.prefs_before_publish.lock().unwrap() = None;
        registry
            .load_prefs_guarded(root.path(), owner.handle())
            .unwrap();
        assert_eq!(registry.enabled_set(), vec![HarnessId::ClaudeCode]);
        drop(owner);
        let replacement =
            DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
                .unwrap();
        let reloaded = HarnessRegistry::new();
        slot(&reloaded, HarnessId::ClaudeCode);
        slot(&reloaded, HarnessId::Codex);
        reloaded
            .load_prefs_guarded(root.path(), replacement.handle())
            .unwrap();
        assert_eq!(reloaded.enabled_set(), vec![HarnessId::ClaudeCode]);
        assert!(!std::fs::read_to_string(path).unwrap().contains("enabled"));
    }

    #[test]
    fn guarded_registry_rejects_foreign_root_and_ticket() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let other_owner =
            DeviceWriteOwner::new(other.path(), InstanceLock::acquire(other.path()).unwrap())
                .unwrap();
        let registry = HarnessRegistry::new();
        slot(&registry, HarnessId::ClaudeCode);
        slot(&registry, HarnessId::Codex);
        assert!(
            registry
                .load_prefs_guarded(root.path(), other_owner.handle())
                .unwrap_err()
                .to_string()
                .contains("outside the admitted device root")
        );
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        registry
            .load_prefs_guarded(root.path(), owner.handle())
            .unwrap();
        let foreign = other_owner.handle().try_admit().unwrap();
        assert!(
            registry
                .set_enabled_with_permit(HarnessId::Codex, false, Some(&foreign))
                .is_err()
        );
        assert_eq!(registry.enabled_set().len(), 2);
        assert!(!root.path().join("harness-prefs.json").exists());
        registry.load_prefs(other.path());
        registry.set_enabled(HarnessId::Codex, false).unwrap();
        assert!(root.path().join("harness-prefs.json").exists());
        assert!(!other.path().join("harness-prefs.json").exists());
    }
}
