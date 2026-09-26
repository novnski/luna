//! Root-instance identity and conservative, local-only legacy association repair.
//! No provider inspection or provider mutation is performed by this module.

use super::*;
use std::path::PathBuf;

use luna_proto::{Chat, CommandReceipt, WorktreeSpec};
use luna_sync::{WorkspaceMaterializationFence, WorkspaceRepairFence};

const ROOT_PREFIX: &str = "workspace:v2:";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceRepairBlocker {
    code: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceRepairPreview {
    pub task_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_digest: Option<String>,
    pub expected_task_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_binding_generation: Option<u32>,
    pub workspace_repair_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_workspace: Option<WorkspaceIdentity>,
    pub blockers: Vec<WorkspaceRepairBlocker>,
}

#[derive(Debug, Clone, Serialize)]
struct RepairEvidence {
    task: LunaTaskIdentity,
    registered_workspace: WorkspaceIdentity,
    chat_device: String,
    chat_cwd: Option<String>,
    current_ui_project: Option<String>,
    current_ui_project_root: Option<String>,
    canonical_root: String,
    task_revision: u64,
    workspace_revision: u64,
}

#[derive(Clone)]
pub(super) struct PendingWorkspaceRepair {
    task_id: String,
    evidence_digest: String,
    expires_at_ms: i64,
    fence: WorkspaceRepairFence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkspaceAssociationRepair {
    task_id: String,
    command_id: String,
    from_workspace_id: String,
    target_workspace: WorkspaceIdentity,
    evidence_digest: String,
    from_task_revision: u64,
    to_task_revision: u64,
    from_workspace_repair_revision: u64,
    to_workspace_repair_revision: u64,
    at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkspaceMaterialization {
    task_id: String,
    command_id: String,
    original_command_hash: String,
    workspace_repair_revision: u64,
    from_workspace_id: String,
    target_workspace: WorkspaceIdentity,
    source_repo: String,
    source_base: String,
    at_ms: i64,
}

/// Opaque, server-created expectation captured before physical worktree creation.
#[derive(Debug, Clone)]
pub(crate) struct WorkspaceMaterializationContext {
    task_id: String,
    workspace: WorkspaceIdentity,
    chat: Chat,
    spec: WorktreeSpec,
    common_git_dir: PathBuf,
}

fn problem(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

pub(super) fn owner_home_root() -> Result<PathBuf, EngineError> {
    let root = crate::repos::home_dir();
    if !root.is_absolute() {
        return Err(problem(
            "Owner home is unavailable; choose an absolute workspace directory",
        ));
    }
    Ok(root)
}

fn checked_root(raw: &str) -> Result<PathBuf, EngineError> {
    let root = input_root(raw)?;
    let canonical = root.canonicalize().map_err(|error| {
        problem(format!(
            "Cannot verify absolute workspace directory {}: {error}",
            root.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(problem(
            "Workspace cwd must name an existing absolute directory",
        ));
    }
    Ok(canonical)
}

fn input_root(raw: &str) -> Result<PathBuf, EngineError> {
    let root = if raw == "~" {
        owner_home_root()?
    } else {
        PathBuf::from(raw)
    };
    if raw.trim().is_empty() || !root.is_absolute() {
        return Err(problem(
            "Workspace cwd must be an absolute directory or owner-local ~; engine process cwd is never used",
        ));
    }
    Ok(root)
}

/// `true` means verified NotFound. Permission, file and ambiguous alias
/// failures remain errors, so a display preview never invents a root identity.
fn import_root_is_missing(raw: &str) -> Result<bool, EngineError> {
    let root = input_root(raw)?;
    match root.canonicalize() {
        Ok(path) if path.is_dir() => return Ok(false),
        Ok(_) => return Err(problem("Historical workspace cwd is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(problem(format!(
                "Historical workspace cannot be verified: {error}"
            )));
        }
    }
    if root.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(problem(
            "Historical workspace has ambiguous unresolved path components",
        ));
    }
    let mut ancestor = root.as_path();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    let resolved = ancestor.canonicalize().map_err(|_| {
                        problem("Historical workspace alias is unresolved; it is not a verified missing directory")
                    })?;
                    if !resolved.is_dir() {
                        return Err(problem(
                            "Historical workspace alias does not name a directory",
                        ));
                    }
                } else if !metadata.is_dir() {
                    return Err(problem(
                        "Historical workspace contains a non-directory component",
                    ));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(parent) = ancestor.parent() else {
                    break;
                };
                ancestor = parent;
            }
            Err(error) => {
                return Err(problem(format!(
                    "Historical workspace ancestor cannot be verified: {error}"
                )));
            }
        }
    }
    Ok(true)
}

fn root_id(environment: &str, origin_space: Option<&str>, root: &str) -> String {
    let encoded = serde_json::json!(["luna-workspace-root-v2", environment, origin_space, root]);
    format!("{ROOT_PREFIX}{}", digest(&encoded.to_string()))
}

fn root_instance(mut workspace: WorkspaceIdentity, canonical: &Path) -> WorkspaceIdentity {
    workspace.schema_version = 2;
    workspace.current_path = canonical.to_string_lossy().into_owned();
    workspace.workspace_id = root_id(
        &workspace.environment_id,
        workspace.source_space_id.as_deref(),
        &workspace.current_path,
    );
    workspace.observed_at_ms = now_ms();
    workspace
}

/// Recheck the task-specific proof under WorkspaceHost's registry lock,
/// immediately before its identity/receipt transaction. Normal task commands
/// advance R; this also catches out-of-band local registry observations.
fn validate_repair_chat_at_commit(
    proof: &RepairEvidence,
    registry: &luna_doc::RegistryDoc,
) -> Result<(), EngineError> {
    let chat = registry
        .chat(&proof.task.task_id)?
        .ok_or_else(|| problem("Task metadata disappeared before repair commit"))?;
    if chat.device_id != proof.chat_device
        || chat.cwd != proof.chat_cwd
        || chat.space_id != proof.current_ui_project
    {
        return Err(problem(
            "Task owner, cwd or UI project changed since repair review",
        ));
    }
    match proof.current_ui_project.as_deref() {
        Some(id) => {
            let space = registry
                .space(id)?
                .ok_or_else(|| problem("Current UI project disappeared before repair commit"))?;
            let expected = proof
                .current_ui_project_root
                .as_deref()
                .ok_or_else(|| problem("Current UI project proof is incomplete"))?;
            if space.device_id != proof.chat_device
                || checked_root(&space.path)?.to_string_lossy() != expected
            {
                return Err(problem(
                    "Current UI project root changed since repair review",
                ));
            }
        }
        None if proof.current_ui_project_root.is_some() => {
            return Err(problem("Projectless workspace proof is inconsistent"));
        }
        None => {}
    }
    Ok(())
}

pub(super) fn validate_root_observation(
    workspaces: &HashMap<String, WorkspaceIdentity>,
    workspace: &WorkspaceIdentity,
) -> Result<(), EngineError> {
    if workspace.workspace_id.starts_with("unresolved:missing:") {
        return Err(problem(
            "A display-only unresolved workspace cannot be persisted",
        ));
    }
    if !workspace.workspace_id.starts_with(ROOT_PREFIX) {
        return Ok(()); // v1 replay must retain its original facts, including collisions.
    }
    if workspace.schema_version != 2
        || !Path::new(&workspace.current_path).is_absolute()
        || workspace.workspace_id
            != root_id(
                &workspace.environment_id,
                workspace.source_space_id.as_deref(),
                &workspace.current_path,
            )
    {
        return Err(problem("Invalid v2 workspace root identity"));
    }
    if let Some(previous) = workspaces.get(&workspace.workspace_id)
        && (previous.environment_id != workspace.environment_id
            || previous.source_space_id != workspace.source_space_id
            || previous.current_path != workspace.current_path)
    {
        return Err(problem(
            "A v2 workspace observation cannot replace its defining root or origin scope",
        ));
    }
    Ok(())
}

impl Projection {
    pub(super) fn apply_workspace_repair(
        &mut self,
        repair: &WorkspaceAssociationRepair,
    ) -> Result<(), EngineError> {
        if let Some(previous) = self.workspace_repairs.get(&repair.task_id)
            && previous.command_id == repair.command_id
        {
            return if previous == repair {
                Ok(())
            } else {
                Err(problem(
                    "Workspace repair command was replayed with conflicting evidence",
                ))
            };
        }
        let task = self
            .tasks
            .get(&repair.task_id)
            .ok_or_else(|| problem("Workspace repair references an unknown task"))?;
        if task.tombstoned
            || task.origin != TaskOrigin::LunaCreated
            || task.workspace_id != repair.from_workspace_id
            || task.current_binding_id.is_some()
            || self
                .bindings_by_task
                .get(&repair.task_id)
                .is_some_and(|ids| !ids.is_empty())
            || self.pending_materializations.contains_key(&repair.task_id)
            || repair.from_task_revision.checked_add(1) != Some(repair.to_task_revision)
            || repair.to_workspace_repair_revision != repair.to_task_revision
            || repair.from_workspace_repair_revision >= repair.to_workspace_repair_revision
            || repair.from_workspace_repair_revision > repair.from_task_revision
        {
            return Err(problem(
                "Workspace association repair lacks an unchanged unbound task/revision boundary",
            ));
        }
        let previous = self
            .workspaces
            .get(&repair.from_workspace_id)
            .ok_or_else(|| problem("Workspace repair has no prior workspace definition"))?;
        if previous.environment_id != repair.target_workspace.environment_id
            || previous.source_space_id != repair.target_workspace.source_space_id
        {
            return Err(problem(
                "Workspace repair cannot change origin environment or source-space provenance",
            ));
        }
        validate_root_observation(&self.workspaces, &repair.target_workspace)?;
        self.workspaces.insert(
            repair.target_workspace.workspace_id.clone(),
            repair.target_workspace.clone(),
        );
        self.tasks.get_mut(&repair.task_id).unwrap().workspace_id =
            repair.target_workspace.workspace_id.clone();
        self.workspace_repairs
            .insert(repair.task_id.clone(), repair.clone());
        Ok(())
    }

    pub(super) fn apply_workspace_materialization(
        &mut self,
        materialization: &WorkspaceMaterialization,
    ) -> Result<(), EngineError> {
        let task = self
            .tasks
            .get(&materialization.task_id)
            .ok_or_else(|| problem("Workspace materialization references an unknown task"))?;
        if task.tombstoned
            || task.origin != TaskOrigin::LunaCreated
            || task.workspace_id != materialization.from_workspace_id
            || !task.workspace_id.starts_with(ROOT_PREFIX)
            || task.current_binding_id.is_some()
            || self
                .bindings_by_task
                .get(&materialization.task_id)
                .is_some_and(|ids| !ids.is_empty())
        {
            return Err(problem(
                "Only an unbound v2 task can initialize a different worktree root",
            ));
        }
        let previous = self
            .workspaces
            .get(&materialization.from_workspace_id)
            .ok_or_else(|| {
                problem("Workspace materialization has no prior workspace definition")
            })?;
        if previous.environment_id != materialization.target_workspace.environment_id
            || previous.source_space_id != materialization.target_workspace.source_space_id
        {
            return Err(problem(
                "Workspace materialization cannot change origin environment or source scope",
            ));
        }
        validate_root_observation(&self.workspaces, &materialization.target_workspace)?;
        self.workspaces.insert(
            materialization.target_workspace.workspace_id.clone(),
            materialization.target_workspace.clone(),
        );
        self.tasks
            .get_mut(&materialization.task_id)
            .unwrap()
            .workspace_id = materialization.target_workspace.workspace_id.clone();
        Ok(())
    }
}

impl TaskIdentityService {
    pub(super) fn workspace_for_import_preview(
        &self,
        space_id: Option<&str>,
        device_id: Option<&str>,
        cwd: &str,
    ) -> Result<(WorkspaceIdentity, bool), EngineError> {
        let owner = if let Some(space_id) = space_id {
            let space = self
                .inner
                .workspace
                .space(space_id)?
                .ok_or_else(|| problem(format!("no such space: {space_id}")))?;
            // An ambiguous selected project must not be hidden by a separate
            // provider cwd that also happens to be missing.
            let _ = import_root_is_missing(&space.path).map_err(|error| {
                problem(format!("Selected project root cannot be verified: {error}"))
            })?;
            space.device_id
        } else {
            device_id.unwrap_or(&self.inner.device_id).to_owned()
        };
        if owner != self.inner.device_id {
            return Err(problem(
                "Historical workspace preview requires its owning environment",
            ));
        }
        if !import_root_is_missing(cwd)? {
            return self
                .resolve_workspace_target(space_id, device_id, Some(cwd))
                .map(|workspace| (workspace, false));
        }
        Ok((
            WorkspaceIdentity {
                schema_version: TASK_IDENTITY_SCHEMA_VERSION,
                workspace_id: format!("unresolved:missing:{}", new_id()),
                environment_id: owner,
                source_space_id: space_id.map(str::to_owned),
                repository_id: None,
                worktree_id: None,
                current_path: cwd.into(),
                observed_at_ms: now_ms(),
            },
            true,
        ))
    }
    pub(crate) async fn prepare_workspace_materialization(
        &self,
        task_id: &str,
        spec: &WorktreeSpec,
    ) -> Result<Option<WorkspaceMaterializationContext>, EngineError> {
        let view = {
            let state = lock(&self.inner.state);
            let Some(view) = state.projection.view(task_id) else {
                return Ok(None);
            };
            let current_w = self.inner.store.workspace_repair_revision(task_id)?;
            self.validate_repair_watermark(task_id, &state.projection, current_w)?;
            view
        };
        if view.task.tombstoned
            || view.task.origin != TaskOrigin::LunaCreated
            || !view.workspace.workspace_id.starts_with(ROOT_PREFIX)
            || self.has_pending_materialization(task_id)
        {
            return Err(problem(
                "Managed workspace needs explicit verified repair/adoption before worktree initialization",
            ));
        }
        let chat = self
            .inner
            .workspace
            .chat(task_id)?
            .ok_or_else(|| problem("Managed task metadata is unavailable"))?;
        let current_root = checked_root(chat.cwd.as_deref().unwrap_or("~"))?;
        if chat.device_id != self.inner.device_id
            || view.workspace.environment_id != self.inner.device_id
            || current_root != checked_root(&view.workspace.current_path)?
            || current_root.to_string_lossy() != view.workspace.current_path
        {
            return Err(problem(
                "Task cwd changed before worktree initialization; no reassociation is authorized",
            ));
        }
        let source_root = checked_root(&spec.repo_path)?;
        let common_git_dir = git_common_dir(&source_root).await?;
        if git_common_dir(&current_root).await? != common_git_dir {
            return Err(problem(
                "WorktreeSpec belongs to a different repository; relocation requires an explicit handoff",
            ));
        }
        if view.current_binding.is_some() || !view.bindings.is_empty() {
            // Bound tasks may reuse their exact linked checkout, never change root.
            verify_linked_checkout(&source_root, &current_root, &common_git_dir).await?;
        }
        Ok(Some(WorkspaceMaterializationContext {
            task_id: task_id.into(),
            workspace: view.workspace,
            chat,
            spec: spec.clone(),
            common_git_dir,
        }))
    }

    pub(crate) async fn commit_workspace_materialization(
        &self,
        context: WorkspaceMaterializationContext,
        command_id: &str,
        fence: &WorkspaceMaterializationFence,
        resolved_cwd: &str,
        branch: Option<&str>,
    ) -> Result<(), TaskProjectionError> {
        let failed = |message: &str| TaskProjectionError::Mutation(problem(message));
        let target_root = checked_root(resolved_cwd).map_err(TaskProjectionError::Mutation)?;
        let source_root =
            checked_root(&context.spec.repo_path).map_err(TaskProjectionError::Mutation)?;
        if git_common_dir(&source_root)
            .await
            .map_err(TaskProjectionError::Mutation)?
            != context.common_git_dir
        {
            return Err(failed(
                "Source repository identity changed during worktree preparation",
            ));
        }
        verify_linked_checkout(&source_root, &target_root, &context.common_git_dir)
            .await
            .map_err(TaskProjectionError::Mutation)?;
        let _permit = self.admit_write().map_err(TaskProjectionError::Mutation)?;
        let mut state = lock(&self.inner.state);
        let view = state
            .projection
            .view(&context.task_id)
            .ok_or_else(|| failed("Managed task identity disappeared"))?;
        let current_w = self
            .inner
            .store
            .workspace_repair_revision(&context.task_id)
            .map_err(EngineError::from)
            .map_err(TaskProjectionError::Mutation)?;
        self.validate_repair_watermark(&context.task_id, &state.projection, current_w)
            .map_err(TaskProjectionError::Mutation)?;
        if current_w != fence.expected_workspace_repair_revision
            || view.task.tombstoned
            || view.workspace.environment_id != self.inner.device_id
            || !view.workspace.workspace_id.starts_with(ROOT_PREFIX)
        {
            return Err(failed("Workspace initialization authority changed"));
        }
        let already_target = checked_root(&view.workspace.current_path)
            .map_err(TaskProjectionError::Mutation)?
            == target_root;
        if checked_root(&view.workspace.current_path)
            .map_err(TaskProjectionError::Mutation)?
            .to_string_lossy()
            != view.workspace.current_path
        {
            return Err(failed(
                "Workspace root alias moved during preparation; explicit relocation is required",
            ));
        }
        if !already_target
            && (view.task.workspace_id != context.workspace.workspace_id
                || view.current_binding.is_some()
                || !view.bindings.is_empty())
        {
            return Err(failed(
                "Another preparation or provider binding changed this workspace; no root was reassigned",
            ));
        }
        let mut projection = state.projection.clone();
        let mut log = state.log.clone();
        let mut bytes = None;
        if !already_target {
            let mut target = root_instance(context.workspace.clone(), &target_root);
            target.worktree_id = Some(digest(&target.current_path));
            let event = TaskIdentityEvent::WorkspaceMaterialized {
                materialization: WorkspaceMaterialization {
                    task_id: context.task_id.clone(),
                    command_id: command_id.into(),
                    original_command_hash: fence.original_command_hash.clone(),
                    workspace_repair_revision: current_w,
                    from_workspace_id: view.task.workspace_id,
                    target_workspace: target,
                    source_repo: source_root.to_string_lossy().into_owned(),
                    source_base: context.spec.base.clone(),
                    at_ms: now_ms(),
                },
            };
            projection
                .apply(&event)
                .map_err(TaskProjectionError::Mutation)?;
            append_identity_events(&mut log, vec![event]).map_err(TaskProjectionError::Mutation)?;
            bytes = Some(
                serde_json::to_vec(&log)
                    .map_err(EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?,
            );
        }
        let task_id = context.task_id.clone();
        let target = target_root.to_string_lossy().into_owned();
        self.inner.workspace.commit_workspace_materialization(command_id,&task_id,fence,
            bytes.as_deref().map(|bytes|(TASK_IDENTITY_LOG_ID,bytes)), |registry| {
                let chat=registry.chat(&task_id)?.ok_or_else(||problem("Task metadata disappeared during worktree initialization"))?;
                if chat.device_id != context.chat.device_id || chat.space_id != context.chat.space_id {
                    return Err(problem("Task ownership/grouping changed during worktree preparation; review the retained checkout"));
                }
                let chat_root=checked_root(chat.cwd.as_deref().unwrap_or("~"))?;
                if already_target {
                    if chat_root != target_root { return Err(problem("Current chat no longer matches the initialized worktree")); }
                    return Ok(());
                }
                if chat.cwd != context.chat.cwd || chat_root != checked_root(&context.workspace.current_path)? {
                    return Err(problem("Task cwd changed during worktree preparation; no stale metadata was published"));
                }
                registry.set_chat_cwd(&task_id,&target)?;
                if let Some(branch)=branch { registry.set_chat_branch(&task_id,branch)?; }
                Ok(())
            })?;
        if !already_target {
            state.log = log;
            state.projection = projection;
        }
        Ok(())
    }

    pub(super) fn resolve_workspace_target(
        &self,
        space_id: Option<&str>,
        device_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<WorkspaceIdentity, EngineError> {
        if let Some(space_id) = space_id {
            let space = self
                .inner
                .workspace
                .read_spaces()?
                .into_iter()
                .find(|space| space.id == space_id)
                .ok_or_else(|| problem(format!("no such space: {space_id}")))?;
            if space.device_id != self.inner.device_id {
                return Err(problem(
                    "same-machine provider task cannot bind to a remote workspace",
                ));
            }
            let source_root = checked_root(&space.path)?;
            let root = checked_root(cwd.unwrap_or(&space.path))?;
            return Ok(root_instance(
                WorkspaceIdentity {
                    schema_version: 2,
                    workspace_id: String::new(),
                    environment_id: space.device_id,
                    source_space_id: Some(space.id),
                    repository_id: space.checkout_id,
                    worktree_id: (root != source_root).then(|| digest(&root.to_string_lossy())),
                    current_path: String::new(),
                    observed_at_ms: now_ms(),
                },
                &root,
            ));
        }
        let owner = device_id.unwrap_or(&self.inner.device_id);
        if owner != self.inner.device_id {
            return Err(problem(
                "same-machine provider task cannot bind to a remote environment",
            ));
        }
        let root = checked_root(cwd.unwrap_or("~"))?;
        Ok(root_instance(
            WorkspaceIdentity {
                schema_version: 2,
                workspace_id: String::new(),
                environment_id: owner.into(),
                source_space_id: None,
                repository_id: None,
                worktree_id: None,
                current_path: String::new(),
                observed_at_ms: now_ms(),
            },
            &root,
        ))
    }

    fn validate_repair_watermark(
        &self,
        task_id: &str,
        projection: &Projection,
        current: u64,
    ) -> Result<(), EngineError> {
        match projection.workspace_repairs.get(task_id) {
            Some(repair) if repair.to_workspace_repair_revision == current => Ok(()),
            None if current == 0 => Ok(()),
            _ => Err(problem(
                "Workspace repair authorization/history disagree; restored or incomplete state must remain fenced",
            )),
        }
    }

    pub(super) fn validate_persisted_workspace_authority(
        &self,
        task_id: &str,
    ) -> Result<(), EngineError> {
        let state = lock(&self.inner.state);
        let current = self.inner.store.workspace_repair_revision(task_id)?;
        self.validate_repair_watermark(task_id, &state.projection, current)
    }

    pub(super) fn validate_execution_workspace(
        &self,
        task_id: &str,
        cwd: &str,
    ) -> Result<(), EngineError> {
        let state = lock(&self.inner.state);
        let Some(view) = state.projection.view(task_id) else {
            return Ok(());
        };
        let current = self.inner.store.workspace_repair_revision(task_id)?;
        self.validate_repair_watermark(task_id, &state.projection, current)?;
        drop(state);
        if view.task.tombstoned {
            return Err(problem("Task is deleted"));
        }
        let requested = checked_root(cwd)?;
        let effective = checked_root(&view.workspace.current_path)?;
        if view.workspace.workspace_id.starts_with(ROOT_PREFIX)
            && effective.to_string_lossy() != view.workspace.current_path
        {
            return Err(problem(
                "Workspace root moved or its alias changed; explicit relocation is required",
            ));
        }
        if requested != effective || view.workspace.environment_id != self.inner.device_id {
            return Err(problem("task workspace changed; explicit resume refused"));
        }
        if let Some(chat) = self.inner.workspace.chat(task_id)? {
            if chat.device_id != self.inner.device_id
                || checked_root(chat.cwd.as_deref().unwrap_or("~"))? != effective
            {
                return Err(problem(
                    "task workspace changed; current chat and execution identity disagree",
                ));
            }
        } else if self.inner.write_handle.get().is_some() {
            return Err(problem(
                "Managed task metadata is unavailable; execution refused",
            ));
        }
        // Current UI project is deliberately not compared with origin source_space_id.
        Ok(())
    }

    async fn repair_evidence(
        &self,
        task_id: &str,
        revision: u64,
        workspace_revision: u64,
    ) -> Result<(RepairEvidence, WorkspaceIdentity), WorkspaceRepairBlocker> {
        let blocked = |code: &str, message: String| WorkspaceRepairBlocker {
            code: code.into(),
            message,
        };
        let (view, registered) = {
            let state = lock(&self.inner.state);
            let view = state.projection.view(task_id).ok_or_else(|| {
                blocked(
                    "missingIdentity",
                    "Managed task/workspace history is unavailable".into(),
                )
            })?;
            self.validate_repair_watermark(task_id, &state.projection, workspace_revision)
                .map_err(|error| blocked("repairHistoryMismatch", error.to_string()))?;
            if view.task.tombstoned
                || view.task.origin != TaskOrigin::LunaCreated
                || state
                    .projection
                    .pending_materializations
                    .contains_key(task_id)
            {
                return Err(blocked("ineligibleTask", "Deleted/imported/pending-adoption tasks cannot use local-only association repair".into()));
            }
            let registered = state
                .projection
                .registration_workspaces
                .get(task_id)
                .cloned()
                .ok_or_else(|| {
                    blocked(
                        "missingHistory",
                        "Original workspace registration is unavailable".into(),
                    )
                })?;
            if view.current_binding.is_some() || !view.bindings.is_empty() {
                return Err(blocked("providerProofRequired", "Current or historical provider bindings require separately reviewed fresh provider-store/native-session proof; localOnly cannot repair them".into()));
            }
            (view, registered)
        };
        let chat = self
            .inner
            .workspace
            .chat(task_id)
            .map_err(|e| blocked("registryUnavailable", e.to_string()))?
            .ok_or_else(|| {
                blocked(
                    "missingChat",
                    "Current authoritative task metadata is unavailable".into(),
                )
            })?;
        if chat.device_id != self.inner.device_id
            || registered.environment_id != self.inner.device_id
        {
            return Err(blocked(
                "environmentMismatch",
                "Registration and current task must belong to this environment".into(),
            ));
        }
        if chat.harness_session_id.is_some() || chat.harness_session_cwd.is_some() {
            return Err(blocked(
                "providerProofRequired",
                "Legacy provider-session metadata is not proof of an unbound task".into(),
            ));
        }
        let historical_root = checked_root(&registered.current_path)
            .map_err(|e| blocked("historicalRootUnavailable", e.to_string()))?;
        if registered.current_path != "~"
            && registered.current_path != historical_root.to_string_lossy()
        {
            return Err(blocked("historicalAliasAmbiguous", "Legacy alias root cannot be proved from its current spelling; explicit verification is required".into()));
        }
        let current_root = checked_root(chat.cwd.as_deref().unwrap_or("~"))
            .map_err(|e| blocked("currentRootUnavailable", e.to_string()))?;
        if historical_root != current_root {
            return Err(blocked("workspaceEvidenceConflict", "Current cwd differs from original registration; genuine relocation is not association repair".into()));
        }
        let ui_root = if let Some(current_space) = chat.space_id.as_deref() {
            let space = self
                .inner
                .workspace
                .read_spaces()
                .map_err(|e| blocked("registryUnavailable", e.to_string()))?
                .into_iter()
                .find(|space| space.id == current_space)
                .ok_or_else(|| {
                    blocked(
                        "currentProjectUnavailable",
                        "Current UI project is unavailable".into(),
                    )
                })?;
            let root = checked_root(&space.path)
                .map_err(|e| blocked("currentProjectUnavailable", e.to_string()))?;
            if space.device_id != self.inner.device_id {
                return Err(blocked("currentProjectConflict", "Current grouping must belong to the owning device and contain the unchanged cwd".into()));
            }
            if !current_root.starts_with(&root) {
                // A linked worktree is normally outside the project folder.
                // Matching space IDs alone cannot prove that a later LWW path
                // update still names the repository that owns that worktree.
                let common = git_common_dir(&root).await.map_err(|_| {
                    blocked(
                        "currentProjectConflict",
                        "Current project no longer contains the task cwd or its linked repository"
                            .into(),
                    )
                })?;
                verify_linked_checkout(&root, &current_root, &common).await.map_err(|_| blocked(
                    "currentProjectConflict", "Task cwd is not a registered worktree of the current project repository".into()))?;
            }
            Some(root.to_string_lossy().into_owned())
        } else {
            None
        };
        let target = root_instance(registered.clone(), &current_root);
        Ok((
            RepairEvidence {
                task: view.task,
                registered_workspace: registered,
                chat_device: chat.device_id,
                chat_cwd: chat.cwd,
                current_ui_project: chat.space_id,
                current_ui_project_root: ui_root,
                canonical_root: current_root.to_string_lossy().into_owned(),
                task_revision: revision,
                workspace_revision,
            },
            target,
        ))
    }

    pub(crate) async fn preview_workspace_identity_repair(
        &self,
        task_id: &str,
        revision: u64,
        workspace_revision: u64,
        quiescence_blocker: Option<String>,
    ) -> Result<WorkspaceRepairPreview, EngineError> {
        let view = self
            .view(task_id)
            .ok_or_else(|| problem("Managed task not found"))?;
        let mut preview = WorkspaceRepairPreview {
            task_id: task_id.into(),
            status: "blocked".into(),
            preview_id: None,
            evidence_digest: None,
            expected_task_revision: revision,
            expected_binding_generation: view
                .current_binding
                .as_ref()
                .map(|binding| binding.generation),
            workspace_repair_revision: workspace_revision,
            target_workspace: None,
            blockers: Vec::new(),
        };
        if view.workspace.workspace_id.starts_with(ROOT_PREFIX) {
            let valid = self.inner.workspace.chat(task_id)?.is_some_and(|chat| {
                !view.task.tombstoned
                    && chat.device_id == self.inner.device_id
                    && view.workspace.environment_id == self.inner.device_id
                    && checked_root(chat.cwd.as_deref().unwrap_or("~")).ok()
                        == checked_root(&view.workspace.current_path).ok()
                    && checked_root(&view.workspace.current_path)
                        .is_ok_and(|root| root.to_string_lossy() == view.workspace.current_path)
            });
            let state = lock(&self.inner.state);
            if valid
                && self
                    .validate_repair_watermark(task_id, &state.projection, workspace_revision)
                    .is_ok()
            {
                preview.status = "unchanged".into();
                preview.target_workspace = Some(view.workspace);
                return Ok(preview);
            }
        }
        let (proof, target) = match self
            .repair_evidence(task_id, revision, workspace_revision)
            .await
        {
            Ok(value) => value,
            Err(blocker) => {
                preview.blockers.push(blocker);
                return Ok(preview);
            }
        };
        if let Some(message) = quiescence_blocker {
            preview.blockers.push(WorkspaceRepairBlocker {
                code: "taskNotQuiescent".into(),
                message,
            });
            return Ok(preview);
        }
        let evidence_digest = digest(&serde_json::to_string(&proof)?);
        let preview_id = new_id();
        let now = now_ms();
        let mut previews = lock(&self.inner.workspace_repair_previews);
        previews.retain(|_, plan| plan.expires_at_ms >= now);
        if previews.len() >= 256 {
            if let Some(oldest) = previews
                .iter()
                .min_by_key(|(_, plan)| plan.expires_at_ms)
                .map(|(id, _)| id.clone())
            {
                previews.remove(&oldest);
            }
        }
        previews.insert(
            preview_id.clone(),
            PendingWorkspaceRepair {
                task_id: task_id.into(),
                evidence_digest: evidence_digest.clone(),
                expires_at_ms: now + PREVIEW_TTL_MS,
                fence: WorkspaceRepairFence {
                    expected_task_revision: revision,
                    expected_workspace_repair_revision: workspace_revision,
                },
            },
        );
        preview.status = "ready".into();
        preview.preview_id = Some(preview_id);
        preview.evidence_digest = Some(evidence_digest);
        preview.target_workspace = Some(target);
        Ok(preview)
    }

    pub(crate) async fn apply_workspace_identity_repair(
        &self,
        task_id: &str,
        command_id: &str,
        preview_id: &str,
        evidence_digest: &str,
    ) -> Result<CommandReceipt, TaskProjectionError> {
        let mutation = |message: &str| TaskProjectionError::Mutation(problem(message));
        let pending = {
            let previews = lock(&self.inner.workspace_repair_previews);
            previews.get(preview_id).cloned()
        }
        .ok_or_else(|| {
            mutation("Workspace repair preview is unavailable; review a fresh preview")
        })?;
        if pending.task_id != task_id
            || pending.evidence_digest != evidence_digest
            || pending.expires_at_ms < now_ms()
        {
            return Err(mutation(
                "Workspace repair preview expired or does not match this task/evidence",
            ));
        }
        let current = self
            .inner
            .store
            .task_command_snapshot(task_id)
            .map_err(EngineError::from)
            .map_err(TaskProjectionError::Mutation)?
            .snapshot;
        let current_w = self
            .inner
            .store
            .workspace_repair_revision(task_id)
            .map_err(EngineError::from)
            .map_err(TaskProjectionError::Mutation)?;
        if current.task_revision != pending.fence.expected_task_revision
            || current_w != pending.fence.expected_workspace_repair_revision
        {
            return Err(mutation(
                "Workspace repair revision changed; review a fresh preview",
            ));
        }
        let (proof, target) = self
            .repair_evidence(task_id, current.task_revision, current_w)
            .await
            .map_err(|blocker| TaskProjectionError::Mutation(problem(blocker.message)))?;
        if digest(
            &serde_json::to_string(&proof)
                .map_err(EngineError::from)
                .map_err(TaskProjectionError::Mutation)?,
        ) != pending.evidence_digest
        {
            return Err(mutation(
                "Workspace repair evidence changed; no association was corrected",
            ));
        }
        let next = current
            .task_revision
            .checked_add(1)
            .filter(|next| *next <= i64::MAX as u64)
            .ok_or_else(|| mutation("Workspace repair revision exhausted"))?;
        let _permit = self.admit_write().map_err(TaskProjectionError::Mutation)?;
        let mut state = lock(&self.inner.state);
        let view = state
            .projection
            .view(task_id)
            .ok_or_else(|| mutation("Task identity disappeared"))?;
        if view.task != proof.task
            || view.current_binding.is_some()
            || !view.bindings.is_empty()
            || state.projection.registration_workspaces.get(task_id)
                != Some(&proof.registered_workspace)
        {
            return Err(mutation(
                "Workspace identity changed during repair preparation",
            ));
        }
        let repair = WorkspaceAssociationRepair {
            task_id: task_id.into(),
            command_id: command_id.into(),
            from_workspace_id: view.task.workspace_id,
            target_workspace: target,
            evidence_digest: pending.evidence_digest,
            from_task_revision: current.task_revision,
            to_task_revision: next,
            from_workspace_repair_revision: current_w,
            to_workspace_repair_revision: next,
            at_ms: now_ms(),
        };
        let event = TaskIdentityEvent::WorkspaceAssociationRepaired { repair };
        let mut projection = state.projection.clone();
        projection
            .apply(&event)
            .map_err(TaskProjectionError::Mutation)?;
        let mut log = state.log.clone();
        append_identity_events(&mut log, vec![event]).map_err(TaskProjectionError::Mutation)?;
        let bytes = serde_json::to_vec(&log)
            .map_err(EngineError::from)
            .map_err(TaskProjectionError::Mutation)?;
        let receipt = self.inner.workspace.commit_workspace_identity_repair(
            command_id,
            task_id,
            &pending.fence,
            None,
            None,
            (TASK_IDENTITY_LOG_ID, &bytes),
            |registry| validate_repair_chat_at_commit(&proof, registry),
        )?;
        state.log = log;
        state.projection = projection;
        lock(&self.inner.workspace_repair_previews).remove(preview_id);
        Ok(receipt)
    }
}

async fn git_metadata(root: &Path, args: &[&str]) -> Result<Vec<u8>, EngineError> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| problem("Git workspace proof output unavailable"))?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        stdout.take(1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        if bytes.len() > 1024 * 1024 {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(problem("Git workspace proof exceeds its output limit"));
        }
        if !child.wait().await?.success() {
            return Err(problem("Git workspace proof is unavailable"));
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| problem("Git workspace proof timed out"))?
}

async fn git_common_dir(root: &Path) -> Result<PathBuf, EngineError> {
    let bytes = git_metadata(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let text = String::from_utf8(bytes).map_err(|_| problem("Git workspace path is not UTF-8"))?;
    let path = PathBuf::from(text.trim());
    if !path.is_absolute() {
        return Err(problem(
            "Git workspace proof did not produce an absolute path",
        ));
    }
    path.canonicalize().map_err(EngineError::from)
}

async fn verify_linked_checkout(
    source: &Path,
    target: &Path,
    common: &Path,
) -> Result<(), EngineError> {
    if !target.join(".git").is_file() || git_common_dir(target).await? != common {
        return Err(problem(
            "Target must be a linked worktree of the original repository",
        ));
    }
    let listed = git_metadata(source, &["worktree", "list", "--porcelain", "-z"]).await?;
    let registered = listed
        .split(|byte| *byte == 0)
        .filter_map(|record| record.strip_prefix(b"worktree "))
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter_map(|path| Path::new(path).canonicalize().ok())
        .any(|path| path == target);
    if !registered {
        return Err(problem(
            "The prepared worktree is not registered with its original repository",
        ));
    }
    Ok(())
}
