//! Durable app-owned task identities and provider-native reconciliation.
//!
//! The authoritative log is one versioned snapshot in the existing SQLite
//! store. Events are appended under the engine's single-instance lock, saved
//! before their projection becomes visible, and replayed on every restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use zeron_harness::codex::native::{
    CodexNativeClient, CodexThreadObservation, NATIVE_CAPABILITIES,
};
use zeron_proto::{
    BindingOrigin, ChatConfig, ExecutionBinding, HarnessId, ImportOwnership, LunaTaskIdentity,
    ProviderImportPreview, ProviderTaskObservation, ReconciliationRecord, ReconciliationState,
    TASK_IDENTITY_SCHEMA_VERSION, TaskIdentityView, TaskLineage, TaskOrigin, WorkspaceIdentity,
};
use zeron_sync::DocsStore;

use crate::workspace_host::{TaskProjectionError, WorkspaceHost};
use crate::{EngineError, new_id, now_ms};

const TASK_IDENTITY_LOG_ID: &str = "luna-task-identity-events-v1";
const PREVIEW_TTL_MS: i64 = 10 * 60 * 1_000;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdentityLog {
    schema_version: u32,
    events: Vec<TaskIdentityEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskMaterializationIntent {
    task_id: String,
    space_id: Option<String>,
    device_id: String,
    config: Option<ChatConfig>,
    cwd: String,
    title: String,
    archived: Option<bool>,
    ownership: ImportOwnership,
    approved_provider: ProviderTaskObservation,
}

impl Default for TaskIdentityLog {
    fn default() -> Self {
        Self {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            events: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum TaskIdentityEvent {
    WorkspaceObserved {
        workspace: WorkspaceIdentity,
    },
    TaskRegistered {
        task: LunaTaskIdentity,
    },
    BindingCreated {
        binding: ExecutionBinding,
    },
    BindingReconciled {
        binding_id: String,
        reconciliation: ReconciliationRecord,
    },
    BindingMaterialized {
        binding_id: String,
        provider_task_id: String,
        reconciliation: ReconciliationRecord,
    },
    BindingReplaced {
        binding_id: String,
        replacement_id: String,
        at_ms: i64,
    },
    TaskOwnershipAdopted {
        task_id: String,
        at_ms: i64,
    },
    MaterializationPlanned {
        intent: TaskMaterializationIntent,
    },
    MaterializationCompleted {
        task_id: String,
        at_ms: i64,
    },
    TaskTombstoned {
        task_id: String,
        external_import_key: Option<String>,
        at_ms: i64,
    },
}

#[derive(Debug, Clone, Default)]
struct Projection {
    workspaces: HashMap<String, WorkspaceIdentity>,
    tasks: HashMap<String, LunaTaskIdentity>,
    bindings: HashMap<String, ExecutionBinding>,
    bindings_by_task: HashMap<String, Vec<String>>,
    live_provider_bindings: HashMap<String, String>,
    external_imports: HashMap<String, String>,
    import_tombstones: HashMap<String, String>,
    pending_materializations: HashMap<String, TaskMaterializationIntent>,
}

impl Projection {
    fn apply(&mut self, event: &TaskIdentityEvent) -> Result<(), EngineError> {
        match event {
            TaskIdentityEvent::WorkspaceObserved { workspace } => {
                self.workspaces
                    .insert(workspace.workspace_id.clone(), workspace.clone());
            }
            TaskIdentityEvent::TaskRegistered { task } => {
                if let Some(existing) = self.tasks.get(&task.task_id) {
                    if existing != task {
                        return Err(EngineError::Other(format!(
                            "task identity {} was registered with conflicting metadata",
                            task.task_id
                        )));
                    }
                    return Ok(());
                }
                if let Some(key) = &task.external_import_key {
                    if let Some(tombstoned) = self.import_tombstones.get(key) {
                        return Err(EngineError::Other(format!(
                            "provider task import is tombstoned by task {tombstoned}"
                        )));
                    }
                    if let Some(existing) = self.external_imports.get(key) {
                        return Err(EngineError::Other(format!(
                            "provider task is already imported as {existing}"
                        )));
                    }
                    self.external_imports
                        .insert(key.clone(), task.task_id.clone());
                }
                self.tasks.insert(task.task_id.clone(), task.clone());
            }
            TaskIdentityEvent::BindingCreated { binding } => {
                let task = self.tasks.get_mut(&binding.task_id).ok_or_else(|| {
                    EngineError::Other(format!(
                        "binding {} references unknown task {}",
                        binding.binding_id, binding.task_id
                    ))
                })?;
                if let Some(existing) = self.bindings.get(&binding.binding_id) {
                    if existing != binding {
                        return Err(EngineError::Other(format!(
                            "binding {} was replayed with conflicting contents",
                            binding.binding_id
                        )));
                    }
                    return Ok(());
                }
                if !binding.provider_task_id.is_empty() {
                    let tuple = provider_tuple(binding);
                    if let Some(existing_id) = self.live_provider_bindings.get(&tuple) {
                        return Err(EngineError::Other(format!(
                            "provider task tuple is already bound by {existing_id}"
                        )));
                    }
                }
                let expected_generation = self
                    .bindings_by_task
                    .get(&binding.task_id)
                    .map_or(1, |ids| ids.len() as u32 + 1);
                if binding.generation != expected_generation {
                    return Err(EngineError::Other(format!(
                        "binding {} has generation {}, expected {expected_generation}",
                        binding.binding_id, binding.generation
                    )));
                }
                if !binding.provider_task_id.is_empty() {
                    self.live_provider_bindings
                        .insert(provider_tuple(binding), binding.binding_id.clone());
                }
                self.bindings_by_task
                    .entry(binding.task_id.clone())
                    .or_default()
                    .push(binding.binding_id.clone());
                task.current_binding_id = Some(binding.binding_id.clone());
                self.bindings
                    .insert(binding.binding_id.clone(), binding.clone());
            }
            TaskIdentityEvent::BindingReconciled {
                binding_id,
                reconciliation,
            } => {
                let binding = self.bindings.get_mut(binding_id).ok_or_else(|| {
                    EngineError::Other(format!(
                        "reconciliation references unknown binding {binding_id}"
                    ))
                })?;
                binding.last_observed_at_ms = reconciliation.observed_at_ms;
                binding.reconciliation = reconciliation.clone();
            }
            TaskIdentityEvent::BindingMaterialized {
                binding_id,
                provider_task_id,
                reconciliation,
            } => {
                if provider_task_id.is_empty() {
                    return Err(EngineError::Other(
                        "binding materialization has an empty provider task id".into(),
                    ));
                }
                let binding = self.bindings.get(binding_id).ok_or_else(|| {
                    EngineError::Other(format!(
                        "materialization references unknown binding {binding_id}"
                    ))
                })?;
                if !binding.provider_task_id.is_empty() {
                    if binding.provider_task_id == *provider_task_id {
                        return Ok(());
                    }
                    return Err(EngineError::Other(format!(
                        "binding {binding_id} is already materialized"
                    )));
                }
                let tuple = provider_tuple_parts(
                    binding.provider_id,
                    &binding.provider_instance_id,
                    provider_task_id,
                );
                if let Some(existing_id) = self.live_provider_bindings.get(&tuple) {
                    return Err(EngineError::Other(format!(
                        "provider task tuple is already bound by {existing_id}"
                    )));
                }
                let binding = self.bindings.get_mut(binding_id).expect("checked above");
                binding.provider_task_id = provider_task_id.clone();
                binding.last_observed_at_ms = reconciliation.observed_at_ms;
                binding.reconciliation = reconciliation.clone();
                self.live_provider_bindings
                    .insert(tuple, binding_id.clone());
            }
            TaskIdentityEvent::BindingReplaced {
                binding_id,
                replacement_id,
                at_ms,
            } => {
                let binding = self.bindings.get_mut(binding_id).ok_or_else(|| {
                    EngineError::Other(format!(
                        "replacement references unknown binding {binding_id}"
                    ))
                })?;
                self.live_provider_bindings.remove(&provider_tuple(binding));
                binding.replaced_by_binding_id = Some(replacement_id.clone());
                binding.last_observed_at_ms = *at_ms;
                binding.reconciliation = ReconciliationRecord {
                    state: ReconciliationState::Replaced,
                    reason: format!("replaced by binding {replacement_id}"),
                    observed_at_ms: *at_ms,
                    provider: binding.reconciliation.provider.clone(),
                };
            }
            TaskIdentityEvent::TaskOwnershipAdopted { task_id, .. } => {
                let task = self.tasks.get_mut(task_id).ok_or_else(|| {
                    EngineError::Other(format!(
                        "ownership transition references unknown task {task_id}"
                    ))
                })?;
                if !matches!(
                    task.origin,
                    TaskOrigin::ProviderImported | TaskOrigin::ProviderAdopted
                ) {
                    return Err(EngineError::Other(format!(
                        "task {task_id} is not a provider import"
                    )));
                }
                task.origin = TaskOrigin::ProviderAdopted;
            }
            TaskIdentityEvent::MaterializationPlanned { intent } => {
                let task = self.tasks.get(&intent.task_id).ok_or_else(|| {
                    EngineError::Other(format!(
                        "materialization references unknown task {}",
                        intent.task_id
                    ))
                })?;
                if task.tombstoned {
                    return Err(EngineError::Other(format!(
                        "tombstoned task {} cannot be materialized",
                        intent.task_id
                    )));
                }
                self.pending_materializations
                    .insert(intent.task_id.clone(), intent.clone());
            }
            TaskIdentityEvent::MaterializationCompleted { task_id, .. } => {
                self.pending_materializations.remove(task_id);
            }
            TaskIdentityEvent::TaskTombstoned {
                task_id,
                external_import_key,
                ..
            } => {
                let task = self.tasks.get_mut(task_id).ok_or_else(|| {
                    EngineError::Other(format!("tombstone references unknown task {task_id}"))
                })?;
                task.tombstoned = true;
                task.current_binding_id = None;
                self.pending_materializations.remove(task_id);
                if let Some(key) = external_import_key {
                    self.external_imports.remove(key);
                    self.import_tombstones.insert(key.clone(), task_id.clone());
                }
                for binding_id in self.bindings_by_task.get(task_id).into_iter().flatten() {
                    if let Some(binding) = self.bindings.get(binding_id) {
                        self.live_provider_bindings.remove(&provider_tuple(binding));
                    }
                }
            }
        }
        Ok(())
    }

    fn view(&self, task_id: &str) -> Option<TaskIdentityView> {
        let task = self.tasks.get(task_id)?.clone();
        let workspace = self.workspaces.get(&task.workspace_id)?.clone();
        let mut bindings = self
            .bindings_by_task
            .get(task_id)
            .into_iter()
            .flatten()
            .filter_map(|id| self.bindings.get(id).cloned())
            .collect::<Vec<_>>();
        bindings.sort_by_key(|binding| binding.generation);
        let current_binding = task
            .current_binding_id
            .as_deref()
            .and_then(|id| self.bindings.get(id))
            .cloned();
        Some(TaskIdentityView {
            task,
            workspace,
            current_binding,
            bindings,
        })
    }
}

/// Replay only the LUNA-owned identity log for an offline backup stage. This
/// does not construct a workspace host or provider client, and cannot trigger
/// startup reconciliation against native provider homes.
pub(crate) fn offline_views_for_backup(
    store: &DocsStore,
) -> Result<Option<HashMap<String, TaskIdentityView>>, EngineError> {
    let Some(bytes) = store.load_snapshot(TASK_IDENTITY_LOG_ID)? else {
        return Ok(None);
    };
    let log: TaskIdentityLog = serde_json::from_slice(&bytes)
        .map_err(|error| EngineError::Other(format!("task identity log decode failed: {error}")))?;
    if log.schema_version != TASK_IDENTITY_SCHEMA_VERSION {
        return Err(EngineError::Other(format!(
            "unsupported task identity schema {}, expected {}",
            log.schema_version, TASK_IDENTITY_SCHEMA_VERSION
        )));
    }
    let mut projection = Projection::default();
    for event in &log.events {
        projection.apply(event)?;
    }
    if !projection.pending_materializations.is_empty() {
        return Err(EngineError::Other(
            "task identity materialization is pending and needs engine recovery".into(),
        ));
    }
    let mut views = HashMap::new();
    for task_id in projection.tasks.keys() {
        let view = projection.view(task_id).ok_or_else(|| {
            EngineError::Other(format!("task identity {task_id} has no replayed workspace"))
        })?;
        views.insert(task_id.clone(), view);
    }
    Ok(Some(views))
}

struct State {
    log: TaskIdentityLog,
    projection: Projection,
}

#[derive(Clone)]
struct PendingPreview {
    preview: ProviderImportPreview,
    fingerprint: String,
    space_id: Option<String>,
    device_id: String,
}

struct Inner {
    store: Arc<DocsStore>,
    workspace: WorkspaceHost,
    state: Mutex<State>,
    previews: Mutex<HashMap<String, PendingPreview>>,
    reconcile_gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    device_id: String,
    codex: CodexNativeClient,
    native_codex_metadata: bool,
    provider_home_identity: String,
    provider_instance_id: String,
    provider_profile_id: String,
}

#[derive(Clone)]
pub struct TaskIdentityService {
    inner: Arc<Inner>,
}

impl TaskIdentityService {
    pub fn open(
        store: Arc<DocsStore>,
        workspace: WorkspaceHost,
        device_id: &str,
        codex_home: PathBuf,
    ) -> Result<Self, EngineError> {
        let codex = CodexNativeClient::new(codex_home);
        Self::open_with_codex_client_and_metadata(store, workspace, device_id, codex, true)
    }

    #[doc(hidden)]
    pub fn open_with_codex_client(
        store: Arc<DocsStore>,
        workspace: WorkspaceHost,
        device_id: &str,
        codex: CodexNativeClient,
    ) -> Result<Self, EngineError> {
        Self::open_with_codex_client_and_metadata(store, workspace, device_id, codex, true)
    }

    /// Hermetic fixture seam: Codex-shaped harness events still exercise the
    /// production binding and context-control paths, while every provider
    /// metadata mutation/read is disabled so no installed executable can be
    /// discovered or spawned.
    #[doc(hidden)]
    pub fn open_without_codex_metadata(
        store: Arc<DocsStore>,
        workspace: WorkspaceHost,
        device_id: &str,
        codex_home: PathBuf,
    ) -> Result<Self, EngineError> {
        Self::open_with_codex_client_and_metadata(
            store,
            workspace,
            device_id,
            CodexNativeClient::new(codex_home),
            false,
        )
    }

    fn open_with_codex_client_and_metadata(
        store: Arc<DocsStore>,
        workspace: WorkspaceHost,
        device_id: &str,
        codex: CodexNativeClient,
        native_codex_metadata: bool,
    ) -> Result<Self, EngineError> {
        let log = match store.load_snapshot(TASK_IDENTITY_LOG_ID)? {
            Some(bytes) => serde_json::from_slice::<TaskIdentityLog>(&bytes).map_err(|error| {
                EngineError::Other(format!("task identity log decode failed: {error}"))
            })?,
            None => TaskIdentityLog::default(),
        };
        if log.schema_version != TASK_IDENTITY_SCHEMA_VERSION {
            return Err(EngineError::Other(format!(
                "unsupported task identity schema {}, expected {}",
                log.schema_version, TASK_IDENTITY_SCHEMA_VERSION
            )));
        }
        let mut projection = Projection::default();
        for event in &log.events {
            projection.apply(event)?;
        }
        // Binding identity follows the canonical shared session store. A
        // selected Codex auth overlay is intentionally absent here: changing
        // credentials must not manufacture a new task/history namespace or
        // weaken the wrong-store guard.
        let (provider_home_identity, provider_instance_id, provider_profile_id) =
            codex_session_store_identity(device_id, codex.codex_home());
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                workspace,
                state: Mutex::new(State { log, projection }),
                previews: Mutex::new(HashMap::new()),
                reconcile_gates: Mutex::new(HashMap::new()),
                device_id: device_id.to_string(),
                codex,
                native_codex_metadata,
                provider_home_identity,
                provider_instance_id,
                provider_profile_id,
            }),
        })
    }

    pub fn start_startup_reconciliation(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(error) = this.recover_pending_materializations().await {
                tracing::warn!(%error, "startup task materialization recovery remains pending");
            }
            for task_id in this.managed_task_ids() {
                if this.has_pending_materialization(&task_id) {
                    continue;
                }
                if let Err(error) = this.reconcile_task(&task_id, false).await {
                    tracing::warn!(task = %task_id, %error, "startup task reconciliation failed");
                }
            }
        });
    }

    pub fn managed_task_ids(&self) -> Vec<String> {
        let state = lock(&self.inner.state);
        let mut ids = state
            .projection
            .tasks
            .values()
            .filter(|task| !task.tombstoned)
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn view(&self, task_id: &str) -> Option<TaskIdentityView> {
        lock(&self.inner.state).projection.view(task_id)
    }

    pub fn current_binding_pending(&self, task_id: &str) -> bool {
        self.view(task_id)
            .and_then(|view| view.current_binding)
            .is_some_and(|binding| binding.provider_task_id.is_empty())
    }

    pub fn has_native_codex_binding(&self, task_id: &str) -> bool {
        self.view(task_id)
            .and_then(|view| view.current_binding)
            .is_some_and(|binding| binding.provider_id == HarnessId::Codex)
    }

    pub fn event_count(&self) -> usize {
        lock(&self.inner.state).log.events.len()
    }

    fn has_pending_materialization(&self, task_id: &str) -> bool {
        lock(&self.inner.state)
            .projection
            .pending_materializations
            .contains_key(task_id)
    }

    pub fn register_new_task(
        &self,
        task_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        cwd: Option<&str>,
        lineage: TaskLineage,
    ) -> Result<bool, EngineError> {
        if uuid::Uuid::parse_str(task_id).is_err() {
            // Compatibility boundary: existing integrations and fixtures use
            // human ids. They remain legacy chats rather than being silently
            // rewritten or adopted into the new identity registry.
            return Ok(false);
        }
        if let Some(existing) = self.view(task_id) {
            if existing.task.tombstoned {
                return Err(EngineError::Other(format!(
                    "task {task_id} is tombstoned and cannot be recreated"
                )));
            }
            if existing.task.lineage != lineage {
                return Err(EngineError::Other(format!(
                    "task {task_id} already has different lineage"
                )));
            }
            return Ok(true);
        }
        let workspace = self.workspace_for_target(space_id, device_id, cwd)?;
        let task = LunaTaskIdentity {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            task_id: task_id.to_string(),
            origin: TaskOrigin::LunaCreated,
            lineage,
            workspace_id: workspace.workspace_id.clone(),
            current_binding_id: None,
            external_import_key: None,
            tombstoned: false,
            created_at_ms: now_ms(),
        };
        self.commit(vec![
            TaskIdentityEvent::WorkspaceObserved { workspace },
            TaskIdentityEvent::TaskRegistered { task },
        ])?;
        Ok(true)
    }

    /// L07 lifecycle boundary for create: stage the identity log in memory,
    /// let the caller commit it with registry/receipt state, and expose the
    /// new identity projection only after that transaction succeeds.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_new_task_with_command<R>(
        &self,
        task_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        cwd: Option<&str>,
        lineage: TaskLineage,
        commit: impl FnOnce(Option<(&str, &[u8])>) -> Result<R, TaskProjectionError>,
    ) -> Result<R, TaskProjectionError> {
        if uuid::Uuid::parse_str(task_id).is_err() {
            return commit(None);
        }
        if let Some(existing) = self.view(task_id) {
            if existing.task.tombstoned {
                return Err(TaskProjectionError::Mutation(EngineError::Other(format!(
                    "task {task_id} is tombstoned and cannot be recreated"
                ))));
            }
            if existing.task.lineage != lineage {
                return Err(TaskProjectionError::Mutation(EngineError::Other(format!(
                    "task {task_id} already has different lineage"
                ))));
            }
            return commit(None);
        }
        let workspace = self
            .workspace_for_target(space_id, device_id, cwd)
            .map_err(TaskProjectionError::Mutation)?;
        let task = LunaTaskIdentity {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            task_id: task_id.to_string(),
            origin: TaskOrigin::LunaCreated,
            lineage,
            workspace_id: workspace.workspace_id.clone(),
            current_binding_id: None,
            external_import_key: None,
            tombstoned: false,
            created_at_ms: now_ms(),
        };
        self.commit_events_with_task_command(
            vec![
                TaskIdentityEvent::WorkspaceObserved { workspace },
                TaskIdentityEvent::TaskRegistered { task },
            ],
            commit,
        )
    }

    /// Stage a fully materialized independent fork identity. The provider
    /// session already passed native readback, so the target binding never
    /// shares the source session and never enters a half-visible pending-id
    /// state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_fork_task_with_command<R>(
        &self,
        source_task_id: &str,
        target_task_id: &str,
        provider_id: HarnessId,
        provider_task_id: &str,
        commit: impl FnOnce(&ExecutionBinding, Option<(&str, &[u8])>) -> Result<R, TaskProjectionError>,
    ) -> Result<R, TaskProjectionError> {
        if uuid::Uuid::parse_str(target_task_id).is_err() {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "fork target must use a stable UUID task id".into(),
            )));
        }
        if provider_task_id.is_empty() {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "provider fork returned an empty session id".into(),
            )));
        }
        if self.view(target_task_id).is_some() {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "fork target task already exists".into(),
            )));
        }
        let source = self.view(source_task_id).ok_or_else(|| {
            TaskProjectionError::Mutation(EngineError::Other(format!(
                "unknown fork source task {source_task_id}"
            )))
        })?;
        if source.task.tombstoned {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "fork source is deleted".into(),
            )));
        }
        let at = now_ms();
        let provider = self.provider_identity(provider_id);
        let task = LunaTaskIdentity {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            task_id: target_task_id.to_string(),
            origin: TaskOrigin::LunaCreated,
            lineage: TaskLineage {
                source_task_id: Some(source_task_id.to_string()),
                lifecycle_owner_task_id: Some(target_task_id.to_string()),
                ..TaskLineage::default()
            },
            workspace_id: source.workspace.workspace_id.clone(),
            current_binding_id: None,
            external_import_key: None,
            tombstoned: false,
            created_at_ms: at,
        };
        let binding = ExecutionBinding {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            binding_id: new_id(),
            task_id: target_task_id.to_string(),
            generation: 1,
            provider_id,
            provider_instance_id: provider.instance_id,
            provider_profile_id: provider.profile_id,
            provider_home_identity: provider.home_identity,
            provider_task_id: provider_task_id.to_string(),
            resume_cursor: None,
            environment_id: source.workspace.environment_id.clone(),
            workspace_id: source.workspace.workspace_id.clone(),
            worktree_id: source.workspace.worktree_id.clone(),
            origin: BindingOrigin::Forked,
            provenance: format!("provider-backed fork of LUNA task {source_task_id}"),
            capabilities: provider.capabilities,
            reconciliation: ReconciliationRecord {
                state: ReconciliationState::Current,
                reason: "provider fork returned a distinct session and native readback succeeded"
                    .into(),
                observed_at_ms: at,
                provider: None,
            },
            replaced_by_binding_id: None,
            created_at_ms: at,
            last_observed_at_ms: at,
        };
        self.commit_events_with_task_command(
            vec![
                TaskIdentityEvent::WorkspaceObserved {
                    workspace: source.workspace,
                },
                TaskIdentityEvent::TaskRegistered { task },
                TaskIdentityEvent::BindingCreated {
                    binding: binding.clone(),
                },
            ],
            |snapshot| commit(&binding, snapshot),
        )
    }

    /// L07 lifecycle boundary for delete. This never deletes provider-private
    /// data; it only stages the LUNA-owned identity tombstone.
    pub(crate) fn tombstone_task_with_command<R>(
        &self,
        task_id: &str,
        commit: impl FnOnce(Option<(&str, &[u8])>) -> Result<R, TaskProjectionError>,
    ) -> Result<R, TaskProjectionError> {
        let Some(view) = self.view(task_id) else {
            return commit(None);
        };
        if view.task.tombstoned {
            return commit(None);
        }
        self.commit_events_with_task_command(
            vec![TaskIdentityEvent::TaskTombstoned {
                task_id: task_id.to_string(),
                external_import_key: view.task.external_import_key,
                at_ms: now_ms(),
            }],
            commit,
        )
    }

    pub fn record_session_binding(
        &self,
        task_id: &str,
        provider_id: HarnessId,
        provider_task_id: &str,
        cwd: &str,
    ) -> Result<Option<ExecutionBinding>, EngineError> {
        if provider_task_id.is_empty() {
            return Err(EngineError::Other(
                "provider returned an empty task id".into(),
            ));
        }
        let Some(mut view) = self.view(task_id) else {
            return Ok(None); // pre-L06 task: preserve it without auto-adoption
        };
        if view.task.tombstoned {
            return Err(EngineError::Other(format!("task {task_id} is tombstoned")));
        }
        let identity = self.provider_identity(provider_id);
        let mut prefix_events = Vec::new();
        if view.workspace.current_path == "~" && Path::new(cwd).is_absolute() {
            view.workspace.current_path = cwd.to_string();
            view.workspace.observed_at_ms = now_ms();
            prefix_events.push(TaskIdentityEvent::WorkspaceObserved {
                workspace: view.workspace.clone(),
            });
        }
        if let Some(current) = view.current_binding {
            if current.provider_task_id.is_empty() {
                if current.provider_id != provider_id
                    || current.provider_instance_id != identity.instance_id
                {
                    return Err(EngineError::Other(format!(
                        "pending provider binding for task {task_id} does not match the accepted session"
                    )));
                }
                let observation = ProviderTaskObservation {
                    provider_task_id: provider_task_id.to_string(),
                    name: None,
                    cwd: cwd.to_string(),
                    status: "active".into(),
                    archived: None,
                    updated_at: None,
                    cli_version: None,
                    project_id: None,
                    ephemeral: false,
                };
                let state = if provider_id == HarnessId::Codex {
                    ReconciliationState::Current
                } else {
                    ReconciliationState::Unsupported
                };
                prefix_events.push(TaskIdentityEvent::BindingMaterialized {
                    binding_id: current.binding_id,
                    provider_task_id: provider_task_id.to_string(),
                    reconciliation: ReconciliationRecord {
                        state,
                        reason: "explicit binding generation materialized from provider acceptance"
                            .into(),
                        observed_at_ms: now_ms(),
                        provider: Some(observation),
                    },
                });
                self.commit(prefix_events)?;
                return Ok(self.view(task_id).and_then(|view| view.current_binding));
            }
            if current.provider_id != provider_id
                || current.provider_instance_id != identity.instance_id
                || current.provider_task_id != provider_task_id
            {
                return Err(EngineError::Other(format!(
                    "refusing silent provider rebind for task {task_id}; create an explicit new generation"
                )));
            }
            let observation = ProviderTaskObservation {
                provider_task_id: provider_task_id.to_string(),
                name: None,
                cwd: cwd.to_string(),
                status: "active".into(),
                archived: None,
                updated_at: None,
                cli_version: None,
                project_id: None,
                ephemeral: false,
            };
            let reconciliation = ReconciliationRecord {
                state: if provider_id == HarnessId::Codex {
                    ReconciliationState::Current
                } else {
                    ReconciliationState::Unsupported
                },
                reason: if provider_id == HarnessId::Codex {
                    "provider session reconnected through its supported runtime".into()
                } else {
                    "provider does not expose a verified native reconciliation API".into()
                },
                observed_at_ms: now_ms(),
                provider: Some(observation),
            };
            prefix_events.push(TaskIdentityEvent::BindingReconciled {
                binding_id: current.binding_id.clone(),
                reconciliation,
            });
            self.commit(prefix_events)?;
            return Ok(self.view(task_id).and_then(|view| view.current_binding));
        }
        let at = now_ms();
        let state = if provider_id == HarnessId::Codex {
            ReconciliationState::Current
        } else {
            ReconciliationState::Unsupported
        };
        let binding = ExecutionBinding {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            binding_id: new_id(),
            task_id: task_id.to_string(),
            generation: 1,
            provider_id,
            provider_instance_id: identity.instance_id,
            provider_profile_id: identity.profile_id,
            provider_home_identity: identity.home_identity,
            provider_task_id: provider_task_id.to_string(),
            resume_cursor: None,
            environment_id: view.workspace.environment_id.clone(),
            workspace_id: view.workspace.workspace_id.clone(),
            worktree_id: view.workspace.worktree_id.clone(),
            origin: BindingOrigin::LunaCreated,
            provenance: "provider SessionStarted accepted by the LUNA engine".into(),
            capabilities: identity.capabilities,
            reconciliation: ReconciliationRecord {
                state,
                reason: if state == ReconciliationState::Current {
                    "new provider session durably bound before SessionStarted publication".into()
                } else {
                    "provider task id retained, but native reconciliation is unsupported".into()
                },
                observed_at_ms: at,
                provider: Some(ProviderTaskObservation {
                    provider_task_id: provider_task_id.to_string(),
                    name: None,
                    cwd: cwd.to_string(),
                    status: "active".into(),
                    archived: None,
                    updated_at: None,
                    cli_version: None,
                    project_id: None,
                    ephemeral: false,
                }),
            },
            replaced_by_binding_id: None,
            created_at_ms: at,
            last_observed_at_ms: at,
        };
        prefix_events.push(TaskIdentityEvent::BindingCreated {
            binding: binding.clone(),
        });
        self.commit(prefix_events)?;
        Ok(Some(binding))
    }

    pub fn validate_explicit_resume(
        &self,
        task_id: &str,
        provider_id: HarnessId,
        cwd: &str,
        provider_task_id: &str,
    ) -> Result<(), EngineError> {
        let Some(view) = self.view(task_id) else {
            return Ok(()); // legacy caller-owned resume
        };
        if view.task.tombstoned {
            return Err(EngineError::Other(format!("task {task_id} is tombstoned")));
        }
        if self.has_pending_materialization(task_id) {
            return Err(EngineError::Other(
                "provider adoption is not fully materialized; restart recovery or retry it before running"
                    .into(),
            ));
        }
        let binding = view
            .current_binding
            .ok_or_else(|| EngineError::Other("managed task has no provider binding".into()))?;
        if binding.provider_id != provider_id {
            return Err(EngineError::Other(format!(
                "task is bound to {:?}, not {provider_id:?}",
                binding.provider_id
            )));
        }
        let current_identity = self.provider_identity(provider_id);
        if binding.provider_instance_id != current_identity.instance_id
            || binding.provider_profile_id != current_identity.profile_id
            || binding.provider_home_identity != current_identity.home_identity
        {
            return Err(EngineError::Other(
                "provider profile/home changed; explicit resume refused".into(),
            ));
        }
        if binding.origin == BindingOrigin::ProviderImported {
            return Err(EngineError::Other(
                "provider-owned imports are read-only until explicitly adopted".into(),
            ));
        }
        if normalized(cwd) != normalized(&view.workspace.current_path) {
            return Err(EngineError::Other(
                "task workspace changed; explicit resume refused".into(),
            ));
        }
        if binding.provider_task_id != provider_task_id {
            return Err(EngineError::Other(format!(
                "explicit resume id does not match the managed provider binding for task {task_id}"
            )));
        }
        Ok(())
    }

    pub async fn prepare_resume(
        &self,
        task_id: &str,
        provider_id: HarnessId,
        cwd: &str,
    ) -> Result<Option<String>, EngineError> {
        let Some(view) = self.view(task_id) else {
            return Ok(None);
        };
        if view.task.tombstoned {
            return Err(EngineError::Other(format!("task {task_id} is tombstoned")));
        }
        if self.has_pending_materialization(task_id) {
            return Err(EngineError::Other(
                "provider adoption is not fully materialized; restart recovery or retry it before running"
                    .into(),
            ));
        }
        let Some(binding) = view.current_binding else {
            return Ok(None);
        };
        if binding.provider_task_id.is_empty() {
            return Ok(None);
        }
        if binding.provider_id != provider_id {
            return Err(EngineError::Other(format!(
                "task is bound to {:?}, not {provider_id:?}",
                binding.provider_id
            )));
        }
        let current_identity = self.provider_identity(provider_id);
        if binding.provider_instance_id != current_identity.instance_id
            || binding.provider_profile_id != current_identity.profile_id
            || binding.provider_home_identity != current_identity.home_identity
        {
            let record = ReconciliationRecord {
                state: ReconciliationState::ExternalChange,
                reason: "provider profile/home differs from the binding; cross-home resume refused"
                    .into(),
                observed_at_ms: now_ms(),
                provider: binding.reconciliation.provider.clone(),
            };
            self.record_reconciliation(&binding.binding_id, record)?;
            return Err(EngineError::Other(
                "provider profile/home changed; refresh and choose an explicit repair".into(),
            ));
        }
        if binding.origin == BindingOrigin::ProviderImported {
            return Err(EngineError::Other(
                "provider-owned imports are read-only until explicitly adopted".into(),
            ));
        }
        if normalized(cwd) != normalized(&view.workspace.current_path) {
            let record = ReconciliationRecord {
                state: ReconciliationState::ExternalChange,
                reason: "requested workspace differs from the binding workspace".into(),
                observed_at_ms: now_ms(),
                provider: binding.reconciliation.provider.clone(),
            };
            self.record_reconciliation(&binding.binding_id, record)?;
            return Err(EngineError::Other(
                "task workspace changed; an explicit binding generation is required".into(),
            ));
        }
        if provider_id != HarnessId::Codex || !self.inner.native_codex_metadata {
            // L06 has no provider-native reconciliation contract for this
            // harness. Keep the binding for ordinary engine-owned continuation
            // without treating the honest `unsupported` native state as a
            // failure of the provider's existing resume mechanism.
            return Ok(Some(binding.provider_task_id));
        }
        let reconciled = self.reconcile_task(task_id, false).await?;
        let binding = reconciled
            .current_binding
            .ok_or_else(|| EngineError::Other("task has no current binding".into()))?;
        if binding.reconciliation.state != ReconciliationState::Current {
            return Err(EngineError::Other(format!(
                "provider resume blocked: {:?}: {}",
                binding.reconciliation.state, binding.reconciliation.reason
            )));
        }
        Ok(Some(binding.provider_task_id))
    }

    pub async fn reconcile_task(
        &self,
        task_id: &str,
        luna_active: bool,
    ) -> Result<TaskIdentityView, EngineError> {
        let gate = {
            let mut gates = lock(&self.inner.reconcile_gates);
            gates
                .entry(task_id.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _guard = gate.lock().await;
        let view = self
            .view(task_id)
            .ok_or_else(|| EngineError::Other(format!("task {task_id} is not L06-managed")))?;
        let binding = view
            .current_binding
            .clone()
            .ok_or_else(|| EngineError::Other(format!("task {task_id} has no provider binding")))?;
        if binding.provider_task_id.is_empty() {
            // Explicit clear/provider-switch generations intentionally have
            // no native id until their first accepted turn materializes them.
            return Ok(view);
        }
        if self.has_pending_materialization(task_id) {
            self.record_reconciliation(
                &binding.binding_id,
                ReconciliationRecord {
                    state: ReconciliationState::Stale,
                    reason: "approved provider import is awaiting local materialization".into(),
                    observed_at_ms: now_ms(),
                    provider: binding.reconciliation.provider.clone(),
                },
            )?;
            return Ok(self.view(task_id).expect("task survives reconciliation"));
        }
        if binding.provider_id != HarnessId::Codex {
            let record = ReconciliationRecord {
                state: ReconciliationState::Unsupported,
                reason: "provider has no verified native-session metadata contract".into(),
                observed_at_ms: now_ms(),
                provider: binding.reconciliation.provider.clone(),
            };
            self.record_reconciliation(&binding.binding_id, record)?;
            return Ok(self.view(task_id).expect("task survives reconciliation"));
        }
        if !self.inner.native_codex_metadata {
            self.record_reconciliation(
                &binding.binding_id,
                ReconciliationRecord {
                    state: ReconciliationState::Unsupported,
                    reason: "synthetic Codex harness has provider metadata access disabled".into(),
                    observed_at_ms: now_ms(),
                    provider: binding.reconciliation.provider.clone(),
                },
            )?;
            return Ok(self.view(task_id).expect("task survives reconciliation"));
        }
        let current = self.provider_identity(HarnessId::Codex);
        if binding.provider_instance_id != current.instance_id
            || binding.provider_profile_id != current.profile_id
            || binding.provider_home_identity != current.home_identity
        {
            self.record_reconciliation(
                &binding.binding_id,
                ReconciliationRecord {
                    state: ReconciliationState::ExternalChange,
                    reason: "current CODEX_HOME/profile does not own this binding".into(),
                    observed_at_ms: now_ms(),
                    provider: binding.reconciliation.provider.clone(),
                },
            )?;
            return Ok(self.view(task_id).expect("task survives reconciliation"));
        }
        if is_missing_environment_path(&view.workspace.current_path) {
            self.record_reconciliation(
                &binding.binding_id,
                ReconciliationRecord {
                    state: ReconciliationState::EnvironmentMissing,
                    reason: format!(
                        "workspace path is unavailable: {}",
                        view.workspace.current_path
                    ),
                    observed_at_ms: now_ms(),
                    provider: binding.reconciliation.provider.clone(),
                },
            )?;
            return Ok(self.view(task_id).expect("task survives reconciliation"));
        }
        let observed = match tokio::time::timeout(
            PROVIDER_TIMEOUT,
            self.inner.codex.observe_thread(&binding.provider_task_id),
        )
        .await
        {
            Ok(Ok(Some(observed))) => observed,
            Ok(Ok(None)) => {
                self.record_reconciliation(
                    &binding.binding_id,
                    ReconciliationRecord {
                        state: ReconciliationState::ProviderMissing,
                        reason: "provider thread was not found in the bound CODEX_HOME".into(),
                        observed_at_ms: now_ms(),
                        provider: None,
                    },
                )?;
                return Ok(self.view(task_id).expect("task survives reconciliation"));
            }
            Ok(Err(error)) => {
                self.record_reconciliation(
                    &binding.binding_id,
                    ReconciliationRecord {
                        state: ReconciliationState::Stale,
                        reason: format!("provider read failed: {error}"),
                        observed_at_ms: now_ms(),
                        provider: binding.reconciliation.provider.clone(),
                    },
                )?;
                return Ok(self.view(task_id).expect("task survives reconciliation"));
            }
            Err(_) => {
                self.record_reconciliation(
                    &binding.binding_id,
                    ReconciliationRecord {
                        state: ReconciliationState::Stale,
                        reason: "provider read timed out".into(),
                        observed_at_ms: now_ms(),
                        provider: binding.reconciliation.provider.clone(),
                    },
                )?;
                return Ok(self.view(task_id).expect("task survives reconciliation"));
            }
        };
        let provider = provider_observation(observed);
        let chat = self.inner.workspace.chat(task_id)?;
        let mut state = ReconciliationState::Current;
        let mut reason = "provider, task, and environment observations agree".to_string();
        if provider.ephemeral {
            state = ReconciliationState::Unsupported;
            reason = "provider thread is ephemeral and not native-app visible".into();
        } else if provider.status == "active" && !luna_active {
            state = ReconciliationState::ForeignActive;
            reason = "provider reports an active turn not owned by this LUNA runtime".into();
        } else if provider.status == "systemError" || provider.status == "unknown" {
            state = ReconciliationState::Stale;
            reason = format!("provider status is {}", provider.status);
        } else if normalized(&provider.cwd) != normalized(&view.workspace.current_path) {
            state = ReconciliationState::ExternalChange;
            reason = format!(
                "provider cwd {} differs from bound workspace {}",
                provider.cwd, view.workspace.current_path
            );
        } else if let Some(previous_project) = binding
            .reconciliation
            .provider
            .as_ref()
            .and_then(|provider| provider.project_id.as_deref())
            && provider.project_id.as_deref() != Some(previous_project)
        {
            state = ReconciliationState::ExternalChange;
            reason = "provider project assignment changed outside LUNA".into();
        } else if binding.origin != BindingOrigin::ProviderImported
            && let Some(chat) = &chat
        {
            let expected_name = provider_title(chat.title.as_deref());
            if provider.name.as_deref() != Some(expected_name.as_str()) {
                state = ReconciliationState::ExternalChange;
                reason = format!(
                    "provider title {:?} differs from expected {expected_name:?}",
                    provider.name
                );
            } else if provider
                .archived
                .is_some_and(|archived| archived != chat.archived)
            {
                state = ReconciliationState::ExternalChange;
                reason = "provider archive state differs from LUNA".into();
            }
        }
        self.record_reconciliation(
            &binding.binding_id,
            ReconciliationRecord {
                state,
                reason,
                observed_at_ms: now_ms(),
                provider: Some(provider),
            },
        )?;
        Ok(self.view(task_id).expect("task survives reconciliation"))
    }

    pub async fn set_provider_name(&self, task_id: &str, title: &str) -> Result<(), EngineError> {
        let binding = self.mutable_codex_binding(task_id)?;
        tokio::time::timeout(
            PROVIDER_TIMEOUT,
            self.inner
                .codex
                .set_name(&binding.provider_task_id, &provider_title(Some(title))),
        )
        .await
        .map_err(|_| EngineError::Other("provider name update timed out".into()))??;
        Ok(())
    }

    pub async fn set_provider_archived(
        &self,
        task_id: &str,
        archived: bool,
    ) -> Result<(), EngineError> {
        let binding = self.mutable_codex_binding(task_id)?;
        tokio::time::timeout(
            PROVIDER_TIMEOUT,
            self.inner
                .codex
                .set_archived(&binding.provider_task_id, archived),
        )
        .await
        .map_err(|_| EngineError::Other("provider archive update timed out".into()))??;
        Ok(())
    }

    pub async fn preview_import(
        &self,
        provider_id: HarnessId,
        provider_task_id: &str,
        ownership: ImportOwnership,
        space_id: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<ProviderImportPreview, EngineError> {
        if !self.inner.native_codex_metadata {
            return Err(EngineError::Other(
                "provider metadata access is disabled for this synthetic runtime".into(),
            ));
        }
        if provider_id != HarnessId::Codex {
            return Err(EngineError::Other(format!(
                "native imports are unsupported for {provider_id:?}"
            )));
        }
        self.sweep_previews();
        let observation = tokio::time::timeout(
            PROVIDER_TIMEOUT,
            self.inner.codex.observe_thread(provider_task_id),
        )
        .await
        .map_err(|_| EngineError::Other("provider import preview timed out".into()))??
        .ok_or_else(|| EngineError::Other("provider task was not found".into()))?;
        let provider_task = provider_observation(observation);
        let workspace =
            self.workspace_for_target(space_id, device_id, Some(provider_task.cwd.as_str()))?;
        let external_key = external_import_key(
            provider_id,
            &self.inner.provider_instance_id,
            provider_task_id,
        );
        let state = lock(&self.inner.state);
        let existing_task_id = state
            .projection
            .external_imports
            .get(&external_key)
            .cloned();
        let tombstoned_by = state
            .projection
            .import_tombstones
            .get(&external_key)
            .cloned();
        let tuple = provider_tuple_parts(
            provider_id,
            &self.inner.provider_instance_id,
            provider_task_id,
        );
        let tuple_conflict = state
            .projection
            .live_provider_bindings
            .get(&tuple)
            .and_then(|id| state.projection.bindings.get(id))
            .map(|binding| binding.task_id.clone());
        drop(state);
        let path_conflict =
            space_id.is_some() && !path_is_within(&provider_task.cwd, &workspace.current_path);
        let (reconciliation_state, conflict) = if let Some(task) = tombstoned_by {
            (
                ReconciliationState::Replaced,
                Some(format!(
                    "this provider task was removed from LUNA as {task}; its tombstone blocks silent re-import"
                )),
            )
        } else if let Some(task) = tuple_conflict.as_ref()
            && existing_task_id.as_ref() != Some(task)
        {
            (
                ReconciliationState::AmbiguousDuplicate,
                Some(format!("provider task is already bound to {task}")),
            )
        } else if provider_task.ephemeral {
            (
                ReconciliationState::Unsupported,
                Some("ephemeral provider tasks cannot be durably imported".into()),
            )
        } else if provider_task.status == "active" {
            (
                ReconciliationState::ForeignActive,
                Some("provider task is currently active in another client".into()),
            )
        } else if path_conflict {
            (
                ReconciliationState::ExternalChange,
                Some(format!(
                    "provider cwd {} does not belong to selected workspace {}",
                    provider_task.cwd, workspace.current_path
                )),
            )
        } else {
            (ReconciliationState::Current, None)
        };
        let title = clean_provider_title(provider_task.name.as_deref());
        let preview = ProviderImportPreview {
            preview_id: new_id(),
            expires_at_ms: now_ms() + PREVIEW_TTL_MS,
            provider_id,
            provider_instance_id: self.inner.provider_instance_id.clone(),
            provider_task,
            ownership,
            workspace,
            proposed_title: title,
            reconciliation_state,
            adoptable: conflict.is_none(),
            conflict,
            existing_task_id,
        };
        let pending = PendingPreview {
            fingerprint: preview_fingerprint(&preview),
            space_id: space_id.map(str::to_string),
            device_id: device_id.unwrap_or(&self.inner.device_id).to_string(),
            preview: preview.clone(),
        };
        lock(&self.inner.previews).insert(preview.preview_id.clone(), pending);
        Ok(preview)
    }

    pub async fn adopt_preview(
        &self,
        preview_id: &str,
        config: Option<ChatConfig>,
    ) -> Result<TaskIdentityView, EngineError> {
        self.sweep_previews();
        let pending = lock(&self.inner.previews)
            .get(preview_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::Other("provider import preview expired or is unknown".into())
            })?;
        if !pending.preview.adoptable {
            return Err(EngineError::Other(
                pending
                    .preview
                    .conflict
                    .clone()
                    .unwrap_or_else(|| "provider import is not adoptable".into()),
            ));
        }
        if pending.preview.provider_instance_id != self.inner.provider_instance_id {
            return Err(EngineError::Other(
                "provider home/profile changed after preview".into(),
            ));
        }
        let refreshed = self
            .preview_import(
                pending.preview.provider_id,
                &pending.preview.provider_task.provider_task_id,
                pending.preview.ownership,
                pending.space_id.as_deref(),
                Some(&pending.device_id),
            )
            .await?;
        if !refreshed.adoptable {
            return Err(EngineError::Other(refreshed.conflict.unwrap_or_else(
                || "provider import is no longer adoptable".into(),
            )));
        }
        if preview_fingerprint(&refreshed) != pending.fingerprint {
            return Err(EngineError::Other(
                "provider task changed after preview; review a fresh preview".into(),
            ));
        }
        let external_key = external_import_key(
            pending.preview.provider_id,
            &pending.preview.provider_instance_id,
            &pending.preview.provider_task.provider_task_id,
        );
        let existing = {
            let state = lock(&self.inner.state);
            state
                .projection
                .external_imports
                .get(&external_key)
                .cloned()
        };
        let task_id = existing.unwrap_or_else(new_id);
        let existing_view = self.view(&task_id);
        let existing_chat = self.inner.workspace.chat(&task_id)?;
        let transition_to_adopted = existing_view
            .as_ref()
            .and_then(|view| view.current_binding.as_ref())
            .is_some_and(|binding| {
                pending.preview.ownership == ImportOwnership::LunaAdopted
                    && binding.origin == BindingOrigin::ProviderImported
            });
        let adopted_state_complete = existing_view
            .as_ref()
            .and_then(|view| view.current_binding.as_ref())
            .is_some_and(|binding| binding.origin == BindingOrigin::ProviderAdopted)
            && existing_chat.as_ref().is_some_and(|chat| {
                chat.title.as_deref() == Some(pending.preview.proposed_title.as_str())
                    && pending
                        .preview
                        .provider_task
                        .archived
                        .is_none_or(|archived| chat.archived == archived)
            })
            && pending.preview.provider_task.name.as_deref()
                == Some(provider_title(Some(&pending.preview.proposed_title)).as_str())
            && self.inner.workspace.chat_harness_session(&task_id)
                == Some((
                    pending.preview.provider_task.provider_task_id.clone(),
                    Some(pending.preview.provider_task.cwd.clone()),
                ));
        let already_complete = existing_view.is_some()
            && existing_chat.is_some()
            && !transition_to_adopted
            && !self.has_pending_materialization(&task_id)
            && match pending.preview.ownership {
                ImportOwnership::ProviderOwned => true,
                ImportOwnership::LunaAdopted => adopted_state_complete,
            };
        if already_complete {
            lock(&self.inner.previews).remove(preview_id);
            return self
                .view(&task_id)
                .ok_or_else(|| EngineError::Other("imported task projection is missing".into()));
        }

        let mut events = Vec::new();
        let at = now_ms();
        if existing_view.is_none() {
            let task_origin = match pending.preview.ownership {
                ImportOwnership::ProviderOwned => TaskOrigin::ProviderImported,
                ImportOwnership::LunaAdopted => TaskOrigin::ProviderAdopted,
            };
            let binding_origin = match pending.preview.ownership {
                ImportOwnership::ProviderOwned => BindingOrigin::ProviderImported,
                ImportOwnership::LunaAdopted => BindingOrigin::ProviderAdopted,
            };
            let task = LunaTaskIdentity {
                schema_version: TASK_IDENTITY_SCHEMA_VERSION,
                task_id: task_id.clone(),
                origin: task_origin,
                lineage: TaskLineage::default(),
                workspace_id: pending.preview.workspace.workspace_id.clone(),
                current_binding_id: None,
                external_import_key: Some(external_key),
                tombstoned: false,
                created_at_ms: at,
            };
            let binding = ExecutionBinding {
                schema_version: TASK_IDENTITY_SCHEMA_VERSION,
                binding_id: new_id(),
                task_id: task_id.clone(),
                generation: 1,
                provider_id: pending.preview.provider_id,
                provider_instance_id: pending.preview.provider_instance_id.clone(),
                provider_profile_id: Some(self.inner.provider_profile_id.clone()),
                provider_home_identity: Some(self.inner.provider_home_identity.clone()),
                provider_task_id: pending.preview.provider_task.provider_task_id.clone(),
                resume_cursor: None,
                environment_id: pending.preview.workspace.environment_id.clone(),
                workspace_id: pending.preview.workspace.workspace_id.clone(),
                worktree_id: pending.preview.workspace.worktree_id.clone(),
                origin: binding_origin,
                provenance: format!("explicit provider import preview {preview_id}"),
                capabilities: NATIVE_CAPABILITIES
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
                reconciliation: ReconciliationRecord {
                    state: ReconciliationState::Current,
                    reason: "provider observation matched the approved import preview".into(),
                    observed_at_ms: at,
                    provider: Some(pending.preview.provider_task.clone()),
                },
                replaced_by_binding_id: None,
                created_at_ms: at,
                last_observed_at_ms: at,
            };
            events.extend([
                TaskIdentityEvent::WorkspaceObserved {
                    workspace: pending.preview.workspace.clone(),
                },
                TaskIdentityEvent::TaskRegistered { task },
                TaskIdentityEvent::BindingCreated { binding },
            ]);
        } else if transition_to_adopted {
            let current = existing_view
                .as_ref()
                .and_then(|view| view.current_binding.as_ref())
                .expect("transition requires a binding");
            let mut replacement = current.clone();
            replacement.binding_id = new_id();
            replacement.generation += 1;
            replacement.origin = BindingOrigin::ProviderAdopted;
            replacement.provenance = format!("explicit provider adoption preview {preview_id}");
            replacement.reconciliation = ReconciliationRecord {
                state: ReconciliationState::Current,
                reason: "provider observation matched the approved adoption preview".into(),
                observed_at_ms: at,
                provider: Some(pending.preview.provider_task.clone()),
            };
            replacement.replaced_by_binding_id = None;
            replacement.created_at_ms = at;
            replacement.last_observed_at_ms = at;
            events.extend([
                TaskIdentityEvent::TaskOwnershipAdopted {
                    task_id: task_id.clone(),
                    at_ms: at,
                },
                TaskIdentityEvent::BindingReplaced {
                    binding_id: current.binding_id.clone(),
                    replacement_id: replacement.binding_id.clone(),
                    at_ms: at,
                },
                TaskIdentityEvent::BindingCreated {
                    binding: replacement,
                },
            ]);
        }

        // A fresh approved preview may supersede an older pending intent after
        // an external provider change. Recovery itself never rewrites intent;
        // only this explicit approval path can do so.
        events.push(TaskIdentityEvent::MaterializationPlanned {
            intent: TaskMaterializationIntent {
                task_id: task_id.clone(),
                space_id: pending.space_id.clone(),
                device_id: pending.device_id.clone(),
                config: config.or_else(|| existing_chat.and_then(|chat| chat.config)),
                cwd: pending.preview.provider_task.cwd.clone(),
                title: pending.preview.proposed_title.clone(),
                archived: pending.preview.provider_task.archived,
                ownership: pending.preview.ownership,
                approved_provider: pending.preview.provider_task.clone(),
            },
        });
        if !events.is_empty() {
            // The task, binding ownership, import key, and enough intent to
            // finish chat materialization survive as one durable decision.
            self.commit(events)?;
        }
        lock(&self.inner.previews).remove(preview_id);
        self.materialize_pending_task(&task_id).await?;
        self.view(&task_id)
            .ok_or_else(|| EngineError::Other("imported task projection is missing".into()))
    }

    pub async fn recover_pending_materializations(&self) -> Result<(), EngineError> {
        let mut task_ids = lock(&self.inner.state)
            .projection
            .pending_materializations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        task_ids.sort();
        if !self.inner.native_codex_metadata && !task_ids.is_empty() {
            return Err(EngineError::Other(
                "pending provider materialization is disabled for this synthetic runtime".into(),
            ));
        }
        let mut first_error = None;
        for task_id in task_ids {
            if let Err(error) = self.materialize_pending_task(&task_id).await {
                tracing::warn!(task = %task_id, %error, "task materialization remains pending");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn materialize_pending_task(&self, task_id: &str) -> Result<(), EngineError> {
        if !self.inner.native_codex_metadata {
            return Err(EngineError::Other(
                "provider materialization is disabled for this synthetic runtime".into(),
            ));
        }
        let intent = lock(&self.inner.state)
            .projection
            .pending_materializations
            .get(task_id)
            .cloned();
        let Some(intent) = intent else {
            return Ok(());
        };
        let view = self
            .view(task_id)
            .ok_or_else(|| EngineError::Other(format!("unknown pending task {task_id}")))?;
        if view.task.tombstoned {
            return Err(EngineError::Other(format!("task {task_id} is tombstoned")));
        }
        let binding = view
            .current_binding
            .ok_or_else(|| EngineError::Other("pending task has no provider binding".into()))?;
        if binding.provider_id != HarnessId::Codex
            || binding.provider_task_id != intent.approved_provider.provider_task_id
        {
            return Err(EngineError::Other(
                "pending adoption no longer matches its provider binding".into(),
            ));
        }
        let current = self.provider_identity(HarnessId::Codex);
        if binding.provider_instance_id != current.instance_id
            || binding.provider_profile_id != current.profile_id
            || binding.provider_home_identity != current.home_identity
            || binding.environment_id != self.inner.device_id
            || binding.environment_id != view.workspace.environment_id
            || binding.workspace_id != view.workspace.workspace_id
            || binding.worktree_id != view.workspace.worktree_id
        {
            self.record_reconciliation(
                &binding.binding_id,
                ReconciliationRecord {
                    state: ReconciliationState::ExternalChange,
                    reason: "current provider home/profile or execution environment does not own the pending adoption; recovery refused before provider access"
                        .into(),
                    observed_at_ms: now_ms(),
                    provider: binding.reconciliation.provider.clone(),
                },
            )?;
            return Err(EngineError::Other(
                "provider home/profile or execution environment changed; pending adoption remains safely blocked for explicit repair"
                    .into(),
            ));
        }
        let observed = tokio::time::timeout(
            PROVIDER_TIMEOUT,
            self.inner.codex.observe_thread(&binding.provider_task_id),
        )
        .await
        .map_err(|_| EngineError::Other("pending adoption provider read timed out".into()))??
        .ok_or_else(|| EngineError::Other("pending adoption provider task is missing".into()))?;
        let mut provider = provider_observation(observed);
        let desired_name = provider_title(Some(&intent.title));
        if !materialization_observation_matches(&intent, &provider, &desired_name) {
            self.record_reconciliation(
                &binding.binding_id,
                ReconciliationRecord {
                    state: ReconciliationState::ExternalChange,
                    reason: "provider task changed after the approved adoption preview".into(),
                    observed_at_ms: now_ms(),
                    provider: Some(provider),
                },
            )?;
            return Err(EngineError::Other(
                "provider task changed after approval; materialization remains safely blocked"
                    .into(),
            ));
        }
        if intent.ownership == ImportOwnership::LunaAdopted
            && provider.name.as_deref() != Some(desired_name.as_str())
        {
            tokio::time::timeout(
                PROVIDER_TIMEOUT,
                self.inner
                    .codex
                    .set_name(&binding.provider_task_id, &desired_name),
            )
            .await
            .map_err(|_| EngineError::Other("provider adoption name update timed out".into()))??;
            provider.name = Some(desired_name);
        }

        if self.inner.workspace.chat(task_id)?.is_none() {
            self.inner.workspace.create_chat(
                task_id,
                intent.space_id.as_deref(),
                Some(&intent.device_id),
                intent.config.clone(),
                Some(intent.cwd.clone()),
            )?;
        }
        let chat = self
            .inner
            .workspace
            .chat(task_id)?
            .ok_or_else(|| EngineError::Other("materialized chat is missing".into()))?;
        if let Some(title) = chat.title.as_deref()
            && title != intent.title
        {
            return Err(EngineError::Other(
                "local task title changed during adoption recovery; refusing to overwrite it"
                    .into(),
            ));
        }
        if !intent.title.is_empty() {
            self.inner.workspace.rename_chat(task_id, &intent.title)?;
        }
        if let Some(archived) = intent.archived {
            self.inner.workspace.set_chat_archived(task_id, archived)?;
        }
        if intent.ownership == ImportOwnership::LunaAdopted {
            self.inner.workspace.set_chat_harness_session(
                task_id,
                &binding.provider_task_id,
                &intent.cwd,
            );
            if self.inner.workspace.chat_harness_session(task_id)
                != Some((binding.provider_task_id.clone(), Some(intent.cwd.clone())))
            {
                return Err(EngineError::Other(
                    "adopted provider session was not persisted on the local task".into(),
                ));
            }
        }
        let at = now_ms();
        self.commit(vec![
            TaskIdentityEvent::BindingReconciled {
                binding_id: binding.binding_id,
                reconciliation: ReconciliationRecord {
                    state: ReconciliationState::Current,
                    reason: "approved provider import is fully materialized".into(),
                    observed_at_ms: at,
                    provider: Some(provider),
                },
            },
            TaskIdentityEvent::MaterializationCompleted {
                task_id: task_id.to_string(),
                at_ms: at,
            },
        ])
    }

    pub fn tombstone_task(&self, task_id: &str) -> Result<(), EngineError> {
        let Some(view) = self.view(task_id) else {
            return Ok(());
        };
        if view.task.tombstoned {
            return Ok(());
        }
        self.commit(vec![TaskIdentityEvent::TaskTombstoned {
            task_id: task_id.to_string(),
            external_import_key: view.task.external_import_key,
            at_ms: now_ms(),
        }])
    }

    pub fn advance_binding(
        &self,
        task_id: &str,
        provider_id: HarnessId,
        provider_task_id: &str,
        origin: BindingOrigin,
        provenance: &str,
    ) -> Result<ExecutionBinding, EngineError> {
        let view = self
            .view(task_id)
            .ok_or_else(|| EngineError::Other(format!("unknown task {task_id}")))?;
        let current = view
            .current_binding
            .ok_or_else(|| EngineError::Other("task has no current binding".into()))?;
        let identity = self.provider_identity(provider_id);
        let at = now_ms();
        let replacement = ExecutionBinding {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            binding_id: new_id(),
            task_id: task_id.to_string(),
            generation: current.generation + 1,
            provider_id,
            provider_instance_id: identity.instance_id,
            provider_profile_id: identity.profile_id,
            provider_home_identity: identity.home_identity,
            provider_task_id: provider_task_id.to_string(),
            resume_cursor: None,
            environment_id: view.workspace.environment_id,
            workspace_id: view.workspace.workspace_id,
            worktree_id: view.workspace.worktree_id,
            origin,
            provenance: provenance.to_string(),
            capabilities: identity.capabilities,
            reconciliation: ReconciliationRecord {
                state: ReconciliationState::Stale,
                reason: "new binding generation awaits provider reconciliation".into(),
                observed_at_ms: at,
                provider: None,
            },
            replaced_by_binding_id: None,
            created_at_ms: at,
            last_observed_at_ms: at,
        };
        self.commit(vec![
            TaskIdentityEvent::BindingReplaced {
                binding_id: current.binding_id,
                replacement_id: replacement.binding_id.clone(),
                at_ms: at,
            },
            TaskIdentityEvent::BindingCreated {
                binding: replacement.clone(),
            },
        ])?;
        Ok(replacement)
    }

    /// Stage an explicit fresh provider binding and let the caller commit the
    /// identity snapshot with registry/chat/receipt state. The provider task
    /// id is deliberately empty until the next SessionStarted event proves
    /// which native session accepted the first prompt.
    pub(crate) fn begin_fresh_binding_with_command<R>(
        &self,
        task_id: &str,
        provider_id: HarnessId,
        origin: BindingOrigin,
        provenance: &str,
        commit: impl FnOnce(&ExecutionBinding, Option<(&str, &[u8])>) -> Result<R, TaskProjectionError>,
    ) -> Result<R, TaskProjectionError> {
        let view = self.view(task_id).ok_or_else(|| {
            TaskProjectionError::Mutation(EngineError::Other(format!("unknown task {task_id}")))
        })?;
        if view.task.tombstoned {
            return Err(TaskProjectionError::Mutation(EngineError::Other(format!(
                "task {task_id} is tombstoned"
            ))));
        }
        let identity = self.provider_identity(provider_id);
        let at = now_ms();
        let generation = view
            .current_binding
            .as_ref()
            .map_or(1, |binding| binding.generation + 1);
        let replacement = ExecutionBinding {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            binding_id: new_id(),
            task_id: task_id.to_string(),
            generation,
            provider_id,
            provider_instance_id: identity.instance_id,
            provider_profile_id: identity.profile_id,
            provider_home_identity: identity.home_identity,
            provider_task_id: String::new(),
            resume_cursor: None,
            environment_id: view.workspace.environment_id,
            workspace_id: view.workspace.workspace_id,
            worktree_id: view.workspace.worktree_id,
            origin,
            provenance: provenance.to_string(),
            capabilities: identity.capabilities,
            reconciliation: ReconciliationRecord {
                state: ReconciliationState::Stale,
                reason: "fresh binding awaits the next provider SessionStarted acceptance".into(),
                observed_at_ms: at,
                provider: None,
            },
            replaced_by_binding_id: None,
            created_at_ms: at,
            last_observed_at_ms: at,
        };
        let mut events = Vec::new();
        if let Some(current) = view.current_binding {
            events.push(TaskIdentityEvent::BindingReplaced {
                binding_id: current.binding_id,
                replacement_id: replacement.binding_id.clone(),
                at_ms: at,
            });
        }
        events.push(TaskIdentityEvent::BindingCreated {
            binding: replacement.clone(),
        });
        self.commit_events_with_task_command(events, |snapshot| commit(&replacement, snapshot))
    }

    /// Stage a replacement binding whose independently forked provider id is
    /// already known and read back. Used by earlier-message editing after the
    /// provider rewind preparation succeeds.
    pub(crate) fn replace_with_forked_binding_with_command<R>(
        &self,
        task_id: &str,
        provider_task_id: &str,
        commit: impl FnOnce(&ExecutionBinding, Option<(&str, &[u8])>) -> Result<R, TaskProjectionError>,
    ) -> Result<R, TaskProjectionError> {
        let view = self.view(task_id).ok_or_else(|| {
            TaskProjectionError::Mutation(EngineError::Other(format!("unknown task {task_id}")))
        })?;
        let current = view.current_binding.ok_or_else(|| {
            TaskProjectionError::Mutation(EngineError::Other(
                "earlier-message edit requires a provider binding".into(),
            ))
        })?;
        if provider_task_id.is_empty() || provider_task_id == current.provider_task_id {
            return Err(TaskProjectionError::Mutation(EngineError::Other(
                "provider rewind did not create an independent session".into(),
            )));
        }
        let at = now_ms();
        let provider = self.provider_identity(current.provider_id);
        let replacement = ExecutionBinding {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            binding_id: new_id(),
            task_id: task_id.to_string(),
            generation: current.generation + 1,
            provider_id: current.provider_id,
            provider_instance_id: provider.instance_id,
            provider_profile_id: provider.profile_id,
            provider_home_identity: provider.home_identity,
            provider_task_id: provider_task_id.to_string(),
            resume_cursor: None,
            environment_id: view.workspace.environment_id,
            workspace_id: view.workspace.workspace_id,
            worktree_id: view.workspace.worktree_id,
            origin: BindingOrigin::Forked,
            provenance: "provider-backed earlier-message edit rewind".into(),
            capabilities: provider.capabilities,
            reconciliation: ReconciliationRecord {
                state: ReconciliationState::Current,
                reason: "staged provider rewind returned a distinct session and native readback succeeded"
                    .into(),
                observed_at_ms: at,
                provider: None,
            },
            replaced_by_binding_id: None,
            created_at_ms: at,
            last_observed_at_ms: at,
        };
        self.commit_events_with_task_command(
            vec![
                TaskIdentityEvent::BindingReplaced {
                    binding_id: current.binding_id,
                    replacement_id: replacement.binding_id.clone(),
                    at_ms: at,
                },
                TaskIdentityEvent::BindingCreated {
                    binding: replacement.clone(),
                },
            ],
            |snapshot| commit(&replacement, snapshot),
        )
    }

    pub async fn compact_task(
        &self,
        task_id: &str,
        codex_home: &Path,
        force_file_auth: bool,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<(), EngineError> {
        let binding = self.mutable_codex_binding(task_id)?;
        let client = self
            .inner
            .codex
            .for_execution_home(codex_home, force_file_auth);
        tokio::time::timeout(
            PROVIDER_TIMEOUT,
            client.compact_thread(&binding.provider_task_id, cwd, model),
        )
        .await
        .map_err(|_| EngineError::Other("Codex compaction completion timed out".into()))?
        .map_err(|error| EngineError::Other(error.to_string()))
    }

    fn mutable_codex_binding(&self, task_id: &str) -> Result<ExecutionBinding, EngineError> {
        if !self.inner.native_codex_metadata {
            return Err(EngineError::Other(
                "provider metadata mutations are disabled for this synthetic runtime".into(),
            ));
        }
        if self.has_pending_materialization(task_id) {
            return Err(EngineError::Other(
                "provider adoption is not fully materialized".into(),
            ));
        }
        let binding = self
            .view(task_id)
            .and_then(|view| view.current_binding)
            .ok_or_else(|| EngineError::Other("task has no current provider binding".into()))?;
        if binding.provider_id != HarnessId::Codex {
            return Err(EngineError::Other(
                "provider-native mutation is unsupported for this task".into(),
            ));
        }
        if binding.origin == BindingOrigin::ProviderImported {
            return Err(EngineError::Other(
                "provider-owned import is read-only until explicitly adopted".into(),
            ));
        }
        if binding.provider_task_id.is_empty() {
            return Err(EngineError::Other(
                "provider binding is waiting for its first accepted turn".into(),
            ));
        }
        let current = self.provider_identity(HarnessId::Codex);
        if binding.provider_instance_id != current.instance_id {
            return Err(EngineError::Other(
                "provider profile/home differs from the task binding".into(),
            ));
        }
        if binding.reconciliation.state != ReconciliationState::Current {
            return Err(EngineError::Other(format!(
                "provider-native mutation requires a current binding, found {:?}: {}",
                binding.reconciliation.state, binding.reconciliation.reason
            )));
        }
        Ok(binding)
    }

    fn record_reconciliation(
        &self,
        binding_id: &str,
        reconciliation: ReconciliationRecord,
    ) -> Result<(), EngineError> {
        self.commit(vec![TaskIdentityEvent::BindingReconciled {
            binding_id: binding_id.to_string(),
            reconciliation,
        }])
    }

    fn commit(&self, events: Vec<TaskIdentityEvent>) -> Result<(), EngineError> {
        let mut state = lock(&self.inner.state);
        let mut projection = state.projection.clone();
        for event in &events {
            projection.apply(event)?;
        }
        let mut log = state.log.clone();
        log.events.extend(events);
        let bytes = serde_json::to_vec(&log).map_err(|error| {
            EngineError::Other(format!("task identity log encode failed: {error}"))
        })?;
        // Durable first: readers never observe an event that restart cannot replay.
        self.inner
            .store
            .save_snapshot(TASK_IDENTITY_LOG_ID, &bytes)?;
        state.log = log;
        state.projection = projection;
        Ok(())
    }

    fn commit_events_with_task_command<R>(
        &self,
        events: Vec<TaskIdentityEvent>,
        commit: impl FnOnce(Option<(&str, &[u8])>) -> Result<R, TaskProjectionError>,
    ) -> Result<R, TaskProjectionError> {
        let mut state = lock(&self.inner.state);
        let mut projection = state.projection.clone();
        for event in &events {
            projection
                .apply(event)
                .map_err(TaskProjectionError::Mutation)?;
        }
        let mut log = state.log.clone();
        log.events.extend(events);
        let bytes = serde_json::to_vec(&log).map_err(|error| {
            TaskProjectionError::Mutation(EngineError::Other(format!(
                "task identity log encode failed: {error}"
            )))
        })?;
        let result = commit(Some((TASK_IDENTITY_LOG_ID, &bytes)))?;
        state.log = log;
        state.projection = projection;
        Ok(result)
    }

    fn workspace_for_target(
        &self,
        space_id: Option<&str>,
        device_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<WorkspaceIdentity, EngineError> {
        let at = now_ms();
        if let Some(space_id) = space_id {
            let space = self
                .inner
                .workspace
                .read_spaces()?
                .into_iter()
                .find(|space| space.id == space_id)
                .ok_or_else(|| EngineError::Other(format!("no such space: {space_id}")))?;
            if space.device_id != self.inner.device_id {
                return Err(EngineError::Other(
                    "same-machine provider task cannot bind to a remote workspace".into(),
                ));
            }
            let current_path = cwd
                .filter(|cwd| !cwd.trim().is_empty())
                .unwrap_or(&space.path)
                .to_string();
            return Ok(WorkspaceIdentity {
                schema_version: TASK_IDENTITY_SCHEMA_VERSION,
                workspace_id: space.id.clone(),
                environment_id: space.device_id.clone(),
                source_space_id: Some(space.id),
                repository_id: space.checkout_id,
                worktree_id: (normalized(&current_path) != normalized(&space.path))
                    .then(|| digest(&current_path)),
                current_path,
                observed_at_ms: at,
            });
        }
        let owner = device_id.unwrap_or(&self.inner.device_id);
        if owner != self.inner.device_id {
            return Err(EngineError::Other(
                "same-machine provider task cannot bind to a remote environment".into(),
            ));
        }
        Ok(WorkspaceIdentity {
            schema_version: TASK_IDENTITY_SCHEMA_VERSION,
            workspace_id: format!("projectless:{owner}"),
            environment_id: owner.to_string(),
            source_space_id: None,
            repository_id: None,
            current_path: cwd.unwrap_or("~").to_string(),
            worktree_id: None,
            observed_at_ms: at,
        })
    }

    fn provider_identity(&self, provider_id: HarnessId) -> ProviderIdentity {
        if provider_id == HarnessId::Codex {
            ProviderIdentity {
                instance_id: self.inner.provider_instance_id.clone(),
                profile_id: Some(self.inner.provider_profile_id.clone()),
                home_identity: Some(self.inner.provider_home_identity.clone()),
                capabilities: NATIVE_CAPABILITIES
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
            }
        } else {
            ProviderIdentity {
                instance_id: format!(
                    "unsupported:{}:{}",
                    provider_name(provider_id),
                    self.inner.device_id
                ),
                profile_id: None,
                home_identity: None,
                capabilities: Vec::new(),
            }
        }
    }

    fn sweep_previews(&self) {
        let now = now_ms();
        lock(&self.inner.previews).retain(|_, pending| pending.preview.expires_at_ms > now);
    }
}

fn codex_session_store_identity(
    device_id: &str,
    canonical_home: &Path,
) -> (String, String, String) {
    let home_identity = digest(&stable_path(canonical_home).to_string_lossy());
    let short = home_identity[..16].to_string();
    (
        home_identity,
        format!("codex:{device_id}:{short}"),
        format!("codex-home:{short}"),
    )
}

struct ProviderIdentity {
    instance_id: String,
    profile_id: Option<String>,
    home_identity: Option<String>,
    capabilities: Vec<String>,
}

fn provider_observation(observation: CodexThreadObservation) -> ProviderTaskObservation {
    ProviderTaskObservation {
        provider_task_id: observation.id,
        name: observation.name,
        cwd: observation.cwd,
        status: observation.status,
        archived: observation.archived,
        updated_at: observation.updated_at,
        cli_version: observation.cli_version,
        project_id: observation.project_id,
        ephemeral: observation.ephemeral,
    }
}

fn provider_tuple(binding: &ExecutionBinding) -> String {
    provider_tuple_parts(
        binding.provider_id,
        &binding.provider_instance_id,
        &binding.provider_task_id,
    )
}

fn provider_tuple_parts(
    provider_id: HarnessId,
    provider_instance_id: &str,
    provider_task_id: &str,
) -> String {
    format!(
        "{}\0{provider_instance_id}\0{provider_task_id}",
        provider_name(provider_id)
    )
}

fn external_import_key(
    provider_id: HarnessId,
    provider_instance_id: &str,
    provider_task_id: &str,
) -> String {
    digest(&provider_tuple_parts(
        provider_id,
        provider_instance_id,
        provider_task_id,
    ))
}

fn provider_name(provider_id: HarnessId) -> String {
    serde_json::to_string(&provider_id)
        .unwrap_or_else(|_| format!("{provider_id:?}"))
        .trim_matches('"')
        .to_string()
}

fn provider_title(title: Option<&str>) -> String {
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    format!("[LUNA] {}", title.unwrap_or("New task"))
}

fn clean_provider_title(title: Option<&str>) -> String {
    title
        .unwrap_or("Imported Codex task")
        .trim()
        .strip_prefix("[LUNA] ")
        .unwrap_or_else(|| title.unwrap_or("Imported Codex task").trim())
        .to_string()
}

fn preview_fingerprint(preview: &ProviderImportPreview) -> String {
    digest(
        &serde_json::to_string(&(
            preview.provider_id,
            &preview.provider_instance_id,
            &preview.provider_task,
            preview.ownership,
            &preview.workspace.workspace_id,
        ))
        .unwrap_or_default(),
    )
}

fn materialization_observation_matches(
    intent: &TaskMaterializationIntent,
    observed: &ProviderTaskObservation,
    desired_name: &str,
) -> bool {
    let approved = &intent.approved_provider;
    let title_matches = observed.name == approved.name
        || (intent.ownership == ImportOwnership::LunaAdopted
            && observed.name.as_deref() == Some(desired_name));
    approved.provider_task_id == observed.provider_task_id
        && normalized(&approved.cwd) == normalized(&observed.cwd)
        && approved.project_id == observed.project_id
        && approved.ephemeral == observed.ephemeral
        && approved.archived == observed.archived
        && observed.status != "active"
        && observed.status != "systemError"
        && observed.status != "unknown"
        && title_matches
}

fn stable_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    // A configured provider home may not exist until the first app-server
    // launch. Canonicalize its nearest existing ancestor so symlinked roots
    // such as macOS `/var` do not change identity after that first launch.
    let mut ancestor = path;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return path.to_path_buf();
        };
        missing.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return path.to_path_buf();
        };
        ancestor = parent;
    }
    let Ok(mut stable) = ancestor.canonicalize() else {
        return path.to_path_buf();
    };
    for component in missing.into_iter().rev() {
        stable.push(component);
    }
    stable
}

fn normalized(path: &str) -> String {
    if path == "~" {
        return path.to_string();
    }
    stable_path(Path::new(path)).to_string_lossy().into_owned()
}

fn is_missing_environment_path(path: &str) -> bool {
    path != "~" && Path::new(path).is_absolute() && !Path::new(path).exists()
}

fn path_is_within(candidate: &str, root: &str) -> bool {
    let candidate = stable_path(Path::new(candidate));
    let root = stable_path(Path::new(root));
    candidate == root || candidate.starts_with(root)
}

fn digest(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_import_key_is_deterministic_but_not_a_task_id() {
        let key = external_import_key(HarnessId::Codex, "instance", "provider-task");
        assert_eq!(
            key,
            external_import_key(HarnessId::Codex, "instance", "provider-task")
        );
        assert!(uuid::Uuid::parse_str(&key).is_err());
        assert_ne!(key, "provider-task");
    }

    #[test]
    fn provider_prefix_is_cleanly_owned() {
        assert_eq!(provider_title(Some("Fix tests")), "[LUNA] Fix tests");
        assert_eq!(clean_provider_title(Some("[LUNA] Fix tests")), "Fix tests");
        assert_eq!(clean_provider_title(Some("Native title")), "Native title");
    }

    #[test]
    fn codex_binding_identity_tracks_canonical_store_not_auth_overlay() {
        let canonical = Path::new("/tmp/luna-canonical-codex-home");
        let first = codex_session_store_identity("device-a", canonical);
        // Two auth overlays use the same canonical continuation store, so the
        // binding tuple is identical. Auth-instance ids live in AgentAccounts.
        let second = codex_session_store_identity("device-a", canonical);
        assert_eq!(first, second);

        let wrong_store =
            codex_session_store_identity("device-a", Path::new("/tmp/other-codex-home"));
        assert_ne!(first.0, wrong_store.0);
        assert_ne!(first.1, wrong_store.1);
        assert_ne!(first.2, wrong_store.2);
    }
}
