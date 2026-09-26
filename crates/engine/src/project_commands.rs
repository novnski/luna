//! Saved local project actions over the existing terminal/process owner.
//! No repository hook discovery and no alternate agent execution path.
use crate::{
    EngineError, Repos, Terminals, WorkspaceHost,
    device_write::{DeviceWriteHandle, DeviceWritePermit},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luna_proto::{
    ProjectAction, ProjectCommandRun, ProjectCommandStatus as Status, ProjectCommandsSnapshot,
    TerminalProcessState,
};
use luna_sync::DocsStore;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct ProjectCommands {
    inner: Arc<Inner>,
}
struct Inner {
    store: Arc<DocsStore>,
    workspace: WorkspaceHost,
    terminals: Terminals,
    repos: Repos,
    device: String,
    writes: DeviceWriteHandle,
    gate: Mutex<()>,
}
struct Target {
    project: String,
    cwd: String,
    generation: String,
}
fn error(text: impl Into<String>) -> EngineError {
    EngineError::Other(text.into())
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl ProjectCommands {
    pub(crate) fn open(
        store: Arc<DocsStore>,
        workspace: WorkspaceHost,
        terminals: Terminals,
        repos: Repos,
        device: String,
        writes: DeviceWriteHandle,
    ) -> Result<Self, EngineError> {
        let service = Self {
            inner: Arc::new(Inner {
                store,
                workspace,
                terminals,
                repos,
                device,
                writes,
                gate: Mutex::new(()),
            }),
        };
        let permit = service.admit()?;
        let projects = service
            .inner
            .workspace
            .read_spaces()?
            .into_iter()
            .filter(|p| p.device_id == service.inner.device)
            .map(|p| p.id)
            .collect::<Vec<_>>();
        service.inner.store.prune_project_commands(
            &service.inner.device,
            &projects,
            Some(permit.ticket()),
        )?;
        // Never replay executable work after boot. Incomplete launch handshakes
        // cannot have run user code; durable running attempts retain their PID.
        for mut run in service.refresh(&permit)? {
            if run.status.is_active() {
                run.status = Status::Canceled;
                run.detail = Some("Engine restarted. Inspect output and retry explicitly; a surviving process group must exit first.".into());
                service.save(&mut run, &permit)?;
            }
        }
        Ok(service)
    }
    fn admit(&self) -> Result<DeviceWritePermit, EngineError> {
        self.inner
            .writes
            .try_admit()
            .map_err(|e| error(e.to_string()))
    }
    fn project(&self, id: &str) -> Result<(), EngineError> {
        if self.inner.store.project_cleanup_state(id)?.is_some() {
            return Err(error(
                "Project deletion is in progress; commands are unavailable",
            ));
        }
        let space = self
            .inner
            .workspace
            .space(id)?
            .ok_or_else(|| error("Project no longer exists"))?;
        if space.device_id != self.inner.device {
            return Err(error(
                "Project commands are saved and executed only on their owning device",
            ));
        }
        Ok(())
    }
    pub fn configuration(&self, project: &str) -> Result<Vec<ProjectAction>, EngineError> {
        self.project(project)?;
        Ok(self
            .inner
            .store
            .project_command_config(project, &self.inner.device)?)
    }
    pub fn configure(&self, project: &str, actions: Vec<ProjectAction>) -> Result<(), EngineError> {
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        self.project(project)?;
        if actions.len() > 24 {
            return Err(error("A project supports up to 24 saved commands"));
        }
        let mut ids = HashSet::new();
        let mut setups = 0;
        for action in &actions {
            if action.id.trim().is_empty()
                || action.id.len() > 128
                || !ids.insert(&action.id)
                || action.name.trim().is_empty()
                || action.name.len() > 120
                || action.command.trim().is_empty()
                || action.command.len() > 16384
                || action.command.contains('\0')
            {
                return Err(error(
                    "Each command needs a unique ID, name and nonempty shell command",
                ));
            }
            if action.setup_on_worktree_create {
                setups += 1;
            }
            if let Some(url) = &action.preview_url {
                let parsed = url::Url::parse(url)
                    .map_err(|_| error("Preview needs a full HTTP or HTTPS URL"))?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(error("Preview needs a full HTTP or HTTPS URL"));
                }
            }
        }
        if setups > 1 {
            return Err(error("Choose only one worktree setup command"));
        }
        self.inner.store.save_project_command_config(
            project,
            &self.inner.device,
            &actions,
            Some(permit.ticket()),
        )?;
        Ok(())
    }
    async fn target(&self, chat_id: &str) -> Result<Target, EngineError> {
        let chat = self
            .inner
            .workspace
            .chat(chat_id)?
            .ok_or_else(|| error("Task no longer exists"))?;
        if chat.device_id != self.inner.device {
            return Err(error("Project commands require a local task"));
        }
        let project = chat
            .space_id
            .ok_or_else(|| error("Select a task in a project"))?;
        self.project(&project)?;
        let path = chat
            .cwd
            .ok_or_else(|| error("Task has no selected checkout"))?;
        let target = self.checkout_target(project, &path).await?;
        self.recheck(chat_id, &target)?;
        Ok(target)
    }
    async fn checkout_target(&self, project: String, path: &str) -> Result<Target, EngineError> {
        if !Path::new(path).is_absolute() {
            return Err(error("Task checkout must be an absolute existing path"));
        }
        let cwd = Path::new(path).canonicalize()?;
        if !cwd.is_dir() {
            return Err(error("Task checkout is unavailable"));
        }
        let identity = match self.inner.repos.checkout_identity(&cwd).await {
            Ok(identity) => Some(identity),
            Err(error) if cwd.ancestors().any(|path| path.join(".git").exists()) => {
                return Err(error);
            }
            Err(_) => None,
        };
        let owner = identity
            .as_ref()
            .map(|id| id.git_dir.as_path())
            .unwrap_or(&cwd);
        let metadata = std::fs::metadata(owner)?;
        let created = metadata
            .created()
            .map_err(|_| {
                error("Cannot establish checkout creation identity; saved commands are unavailable")
            })?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| error("Invalid checkout creation identity"))?;
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        };
        #[cfg(not(unix))]
        let inode = 0u64;
        let generation = digest(
            format!(
                "{}\0{}\0{}\0{}",
                owner.display(),
                identity
                    .as_ref()
                    .map(|id| id.id.as_str())
                    .unwrap_or_default(),
                created.as_nanos(),
                inode
            )
            .as_bytes(),
        );
        let target = Target {
            project,
            cwd: cwd.to_string_lossy().into(),
            generation,
        };
        Ok(target)
    }
    fn recheck(&self, chat_id: &str, target: &Target) -> Result<(), EngineError> {
        self.project(&target.project)?;
        if self.inner.store.task_cleanup_state(chat_id)?.is_some() {
            return Err(error(
                "Task deletion is in progress; commands are unavailable",
            ));
        }
        let chat = self
            .inner
            .workspace
            .chat(chat_id)?
            .ok_or_else(|| error("Task no longer exists"))?;
        let cwd = chat.cwd.as_deref().map(std::fs::canonicalize).transpose()?;
        if chat.device_id != self.inner.device
            || chat.space_id.as_deref() != Some(target.project.as_str())
            || cwd.as_deref() != Some(Path::new(&target.cwd))
        {
            return Err(error(
                "Task ownership or selected checkout changed; reload commands",
            ));
        }
        Ok(())
    }
    fn runs(&self) -> Result<Vec<ProjectCommandRun>, EngineError> {
        Ok(self.inner.store.project_command_runs(&self.inner.device)?)
    }
    fn save(
        &self,
        run: &mut ProjectCommandRun,
        permit: &DeviceWritePermit,
    ) -> Result<(), EngineError> {
        run.updated_at_ms = now();
        if !self.inner.store.save_project_command_run(
            run,
            Some(run.attempt),
            Some(permit.ticket()),
        )? {
            return Err(error("Command attempt changed; reload its status"));
        }
        Ok(())
    }
    fn refresh(&self, permit: &DeviceWritePermit) -> Result<Vec<ProjectCommandRun>, EngineError> {
        let inventory = self.inner.terminals.list(None)?;
        let mut runs = self.runs()?;
        for run in &mut runs {
            if run.status == Status::Running {
                if let Some(terminal) = inventory
                    .iter()
                    .find(|t| Some(t.id.as_str()) == run.terminal_id.as_deref())
                {
                    run.status = match terminal.state {
                        TerminalProcessState::Running => continue,
                        TerminalProcessState::Exited => {
                            if terminal.exit_code == Some(0) {
                                Status::Succeeded
                            } else {
                                Status::Failed
                            }
                        }
                        _ => Status::Canceled,
                    };
                    run.exit_code = terminal.exit_code;
                    run.detail = if run.status == Status::Failed {
                        Some(format!(
                            "Command exited with code {}",
                            terminal.exit_code.unwrap_or(-1)
                        ))
                    } else {
                        None
                    };
                    self.save(run, permit)?;
                } else {
                    run.status = Status::Canceled;
                    run.detail =
                        Some("Terminal is no longer available; inspect before retrying".into());
                    self.save(run, permit)?;
                }
            }
        }
        Ok(runs)
    }
    pub async fn snapshot(&self, chat: &str) -> Result<ProjectCommandsSnapshot, EngineError> {
        let target = self.target(chat).await?;
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        self.recheck(chat, &target)?;
        let runs = self
            .refresh(&permit)?
            .into_iter()
            .filter(|r| {
                r.project_id == target.project
                    && r.checkout_generation == target.generation
                    && (r.chat_id == chat || r.setup)
            })
            .collect();
        Ok(ProjectCommandsSnapshot {
            actions: self.configuration(&target.project)?,
            project_id: target.project,
            cwd: target.cwd,
            runs,
        })
    }
    fn launch(
        &self,
        mut run: ProjectCommandRun,
        permit: &DeviceWritePermit,
    ) -> Result<ProjectCommandRun, EngineError> {
        let handshake = crate::new_id();
        let spawned = self.inner.terminals.open_command_for_task(
            &run.chat_id,
            &run.cwd,
            &run.action.command,
            &handshake,
        );
        match spawned {
            Ok(terminal) => {
                run.terminal_id = Some(terminal.id.clone());
                run.process_group_id = self.inner.terminals.process_id(&terminal.id);
                run.status = Status::Running;
                if let Err(error) = self.save(&mut run, permit) {
                    let _ = self.inner.terminals.close(&terminal.id);
                    return Err(error);
                }
                // PTY sees the authorization only AFTER receipt/PID commit.
                if let Err(error) = self
                    .inner
                    .terminals
                    .write(&terminal.id, &BASE64.encode(format!("{handshake}\n")))
                {
                    let _ = self.inner.terminals.close(&terminal.id);
                    run.status = Status::Failed;
                    run.detail = Some(error.to_string());
                    self.save(&mut run, permit)?;
                }
            }
            Err(error) => {
                run.status = Status::Failed;
                run.detail = Some(error.to_string());
                self.save(&mut run, permit)?;
            }
        }
        Ok(run)
    }
    fn admit_run(
        &self,
        chat: &str,
        target: &Target,
        action: ProjectAction,
        id: &str,
        setup: bool,
        permit: &DeviceWritePermit,
    ) -> Result<ProjectCommandRun, EngineError> {
        if id.is_empty() || id.len() > 256 {
            return Err(error("Invalid command request ID"));
        }
        let action_digest = digest(&serde_json::to_vec(&action).map_err(|e| error(e.to_string()))?);
        if let Some(old) = self.runs()?.into_iter().find(|r| r.id == id) {
            if old.project_id != target.project
                || old.checkout_generation != target.generation
                || (!setup && (old.chat_id != chat || old.action_digest != action_digest))
            {
                return Err(error(
                    "Command request ID already belongs to another execution",
                ));
            }
            return Ok(old);
        }
        let run = ProjectCommandRun {
            id: id.into(),
            project_id: target.project.clone(),
            device_id: self.inner.device.clone(),
            chat_id: chat.into(),
            cwd: target.cwd.clone(),
            checkout_generation: target.generation.clone(),
            action,
            action_digest,
            setup,
            attempt: 1,
            status: Status::Pending,
            terminal_id: None,
            previous_terminal_ids: vec![],
            process_group_id: None,
            exit_code: None,
            detail: None,
            updated_at_ms: now(),
        };
        if !self
            .inner
            .store
            .save_project_command_run(&run, None, Some(permit.ticket()))?
        {
            return Err(error("Command was concurrently admitted; reload"));
        }
        Ok(run)
    }
    fn launch_pending(
        &self,
        run: ProjectCommandRun,
        permit: &DeviceWritePermit,
    ) -> Result<ProjectCommandRun, EngineError> {
        if run.status == Status::Pending {
            self.launch(run, permit)
        } else {
            Ok(run)
        }
    }
    pub async fn run(
        &self,
        chat: &str,
        action_id: &str,
        request_id: &str,
    ) -> Result<ProjectCommandRun, EngineError> {
        let target = self.target(chat).await?;
        let action = self
            .configuration(&target.project)?
            .into_iter()
            .find(|a| a.id == action_id)
            .ok_or_else(|| error("Saved command no longer exists"))?;
        if action.wait_for_setup {
            self.wait_for_setup(chat, &target.cwd).await?;
        }
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        self.recheck(chat, &target)?;
        let run = self.admit_run(chat, &target, action, request_id, false, &permit)?;
        self.launch_pending(run, &permit)
    }
    /// Reserve setup BEFORE publishing a new checkout to the chat. If the
    /// engine dies after the path stamp, the canceled receipt still gates it.
    #[doc(hidden)] // shared materialization seam, exposed for process-level acceptance
    pub async fn prepare_new_worktree(&self, chat: &str, cwd: &str) -> Result<(), EngineError> {
        if self
            .inner
            .workspace
            .chat(chat)?
            .and_then(|c| c.space_id)
            .is_none()
        {
            return Ok(());
        }
        let original = self.target(chat).await?;
        let Some(action) = self
            .configuration(&original.project)?
            .into_iter()
            .find(|a| a.setup_on_worktree_create)
        else {
            return Ok(());
        };
        let target = self.checkout_target(original.project.clone(), cwd).await?;
        let id = format!(
            "setup-{}",
            digest(format!("{}\0{}", target.project, target.generation).as_bytes())
        );
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        self.recheck(chat, &original)?;
        self.admit_run(chat, &target, action, &id, true, &permit)?;
        Ok(())
    }
    /// Called ONLY for a newly materialized worktree, after the existing chat
    /// receipt is stamped with its path. Navigation/config edits never call it.
    pub async fn setup_new_worktree(
        &self,
        chat: &str,
        cwd: &str,
    ) -> Result<Option<ProjectCommandRun>, EngineError> {
        if self
            .inner
            .workspace
            .chat(chat)?
            .and_then(|c| c.space_id)
            .is_none()
        {
            return Ok(None);
        }
        let target = self.target(chat).await?;
        if Path::new(cwd).canonicalize()? != Path::new(&target.cwd) {
            return Err(error("Setup checkout does not match the task"));
        }
        let id = format!(
            "setup-{}",
            digest(format!("{}\0{}", target.project, target.generation).as_bytes())
        );
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        self.recheck(chat, &target)?;
        // A config edit between reservation and checkout publication cannot
        // erase or replace the captured setup dependency.
        if let Some(run) = self.runs()?.into_iter().find(|run| run.id == id) {
            return self.launch_pending(run, &permit).map(Some);
        }
        let Some(action) = self
            .configuration(&target.project)?
            .into_iter()
            .find(|a| a.setup_on_worktree_create)
        else {
            return Ok(None);
        };
        let run = self.admit_run(chat, &target, action, &id, true, &permit)?;
        self.launch_pending(run, &permit).map(Some)
    }
    pub async fn wait_for_setup(&self, chat: &str, cwd: &str) -> Result<(), EngineError> {
        // Projectless tasks continue through the existing normal run path.
        if self
            .inner
            .workspace
            .chat(chat)?
            .and_then(|c| c.space_id)
            .is_none()
        {
            return Ok(());
        }
        if !self.runs()?.iter().any(|run| run.setup) {
            return Ok(());
        }
        let target = self.target(chat).await?;
        if Path::new(cwd).canonicalize()? != Path::new(&target.cwd) {
            return Err(error("Selected checkout changed before setup gate"));
        }
        loop {
            let state = {
                let _gate = self
                    .inner
                    .gate
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let permit = self.admit()?;
                self.recheck(chat, &target)?;
                self.refresh(&permit)?.into_iter().find(|r| {
                    r.setup
                        && r.project_id == target.project
                        && r.checkout_generation == target.generation
                })
            };
            match state {
                None => return Ok(()),
                Some(run) if !run.action.wait_for_setup || run.status == Status::Succeeded => {
                    return Ok(());
                }
                Some(run) if !run.status.is_active() => {
                    return Err(error(format!(
                        "Worktree setup {}. Open Terminal → Commands to inspect output and retry setup, then retry the task.",
                        run.status.label().to_lowercase()
                    )));
                }
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
    fn matching(
        &self,
        chat: &str,
        id: &str,
        target: &Target,
        permit: &DeviceWritePermit,
    ) -> Result<ProjectCommandRun, EngineError> {
        self.recheck(chat, target)?;
        let run = self
            .refresh(permit)?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| error("Command receipt no longer exists"))?;
        if run.project_id != target.project
            || run.cwd != target.cwd
            || run.checkout_generation != target.generation
            || (!run.setup && run.chat_id != chat)
        {
            return Err(error(
                "Command belongs to a different task or checkout; return to its original task",
            ));
        }
        Ok(run)
    }
    pub async fn stop(&self, chat: &str, id: &str) -> Result<ProjectCommandRun, EngineError> {
        let target = self.target(chat).await?;
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        let mut run = self.matching(chat, id, &target, &permit)?;
        if !run.status.is_active() {
            return Ok(run);
        }
        if let Some(terminal) = &run.terminal_id {
            self.inner.terminals.close(terminal)?;
        }
        run.status = Status::Canceled;
        run.detail = Some("Stopped by you".into());
        self.save(&mut run, &permit)?;
        Ok(run)
    }
    pub async fn retry(&self, chat: &str, id: &str) -> Result<ProjectCommandRun, EngineError> {
        let previous = self
            .runs()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| error("Command receipt no longer exists"))?;
        if !previous.setup && previous.action.wait_for_setup {
            self.wait_for_setup(chat, &previous.cwd).await?;
        }
        let target = self.target(chat).await?;
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let permit = self.admit()?;
        let mut run = self.matching(chat, id, &target, &permit)?;
        if run.status.is_active() {
            return Err(error("Command is still running; stop it before retrying"));
        }
        if run.process_group_id.is_some_and(group_alive) {
            return Err(error(
                "Previous command process group still exists. Wait for it to exit before retrying; Luna will not signal a potentially reused process ID.",
            ));
        }
        let previous = run.attempt;
        // A setup receipt belongs to its checkout; a surviving task in that
        // checkout owns a retried terminal after the original task was removed.
        run.chat_id = chat.to_string();
        run.attempt += 1;
        run.status = Status::Pending;
        if let Some(id) = run.terminal_id.take() {
            run.previous_terminal_ids.push(id);
        }
        run.process_group_id = None;
        run.exit_code = None;
        run.detail = None;
        run.updated_at_ms = now();
        if !self.inner.store.save_project_command_run(
            &run,
            Some(previous),
            Some(permit.ticket()),
        )? {
            return Err(error("Attempt changed; reload before retrying"));
        }
        self.launch(run, &permit)
    }
}
fn group_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(-(pid as i32), 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
