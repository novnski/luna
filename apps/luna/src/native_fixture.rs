//! Opt-in local native QA for the shipped Mach-O, with no provider or Edge path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, WorkspaceScope,
    local_adoption_mock_registry,
};
use luna_proto::{ChatConfig, SandboxLevel, Session, SessionStatus};

const MARKER: &str = ".luna-l32-native-fixture";
const MARKER_CONTENTS: &[u8] = b"L32 synthetic native fixture\n";
const PREPARED: &str = ".luna-l32-prepared";
const PREPARED_CONTENTS: &[u8] = b"L32 prepared synthetic root v1\n";
const QA_BUNDLE: &str = "LUNA-Native-QA.app";
const QA_PROJECT_ID: &str = "l32-synthetic-project";
const QA_TASK_ID: &str = "l32-synthetic-mock-task";
const MAX_ENTRIES: usize = 10_000;

/// No-argument LaunchServices route for the exact copied, signed QA bundle.
/// A QA-named bundle in an invalid location is an error, never a production
/// fallback. Ordinary `LUNA.app` paths are left to the normal CLI/startup.
pub fn qa_root_from_executable(executable: &Path) -> Result<Option<PathBuf>> {
    let Some(bundle) = executable
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == QA_BUNDLE))
    else {
        return Ok(None);
    };
    ensure!(
        executable == bundle.join("Contents/MacOS/luna"),
        "QA bundle executable path is malformed"
    );
    ensure!(
        std::fs::symlink_metadata(bundle)?.file_type().is_dir()
            && std::fs::symlink_metadata(executable)?.file_type().is_file(),
        "QA bundle or executable is not a real file"
    );
    let root = validate_root(bundle.parent().context("QA bundle has no fixture root")?)?;
    ensure!(
        bundle.canonicalize()? == root.join(QA_BUNDLE)
            && executable.canonicalize()? == root.join(QA_BUNDLE).join("Contents/MacOS/luna"),
        "QA bundle escaped its fixture root"
    );
    let prepared = root.join(PREPARED);
    ensure!(
        std::fs::symlink_metadata(&prepared)?.file_type().is_file()
            && std::fs::read(&prepared)? == PREPARED_CONTENTS,
        "QA bundle requires a prepared synthetic fixture"
    );
    Ok(Some(root))
}

/// Only an explicitly marked private directory below the system temp root.
pub fn validate_root(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).context("fixture root does not exist")?;
    ensure!(
        metadata.file_type().is_dir(),
        "fixture root must be a real directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "fixture root must be private (mode 0700)"
        );
    }
    let root = path.canonicalize()?;
    let temporary = std::env::temp_dir().canonicalize()?;
    ensure!(
        root != temporary && root.starts_with(&temporary),
        "fixture root must be a child of the system temporary directory"
    );
    let marker = root.join(MARKER);
    ensure!(
        std::fs::symlink_metadata(&marker)?.file_type().is_file()
            && std::fs::read(&marker)? == MARKER_CONTENTS,
        "fixture marker is missing or invalid"
    );
    match std::fs::symlink_metadata(root.join(PREPARED)) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_file()
                && std::fs::read(root.join(PREPARED))? == PREPARED_CONTENTS,
            "prepared fixture marker is invalid"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let children = std::fs::read_dir(&root)?.collect::<std::io::Result<Vec<_>>>()?;
            ensure!(
                children.len() == 1,
                "a new fixture root must contain only its marker"
            );
        }
        Err(error) => return Err(error.into()),
    }
    let mut pending = vec![root.clone()];
    let mut seen = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            seen += 1;
            ensure!(seen <= MAX_ENTRIES, "fixture tree is too large");
            let kind = std::fs::symlink_metadata(entry.path())?.file_type();
            ensure!(
                kind.is_file() || kind.is_dir(),
                "fixture tree contains a link or special file"
            );
            if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(root)
}

fn prepare_core(root: &Path) -> Result<Arc<EngineCore>> {
    let data = root.join("data");
    std::fs::create_dir_all(&data)?;
    let accounts = AgentAccountsConfig {
        data_dir: data.clone(),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor/auth.json"),
    };
    accounts.disable_claude_keychain_fallback()?;
    let mut auth_config = AuthConfig::new(luna_engine::OFFLINE_EDGE_URL, &data);
    auth_config.offline_only = true;
    auth_config.use_in_memory_credentials_for_tests();
    let profile = EngineProfile::local(&data)?;
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile,
        Arc::new(local_adoption_mock_registry()),
        HarnessId::Mock,
        None,
        accounts,
    )?;
    core.set_auth(Auth::new(auth_config));
    ensure!(
        core.workspace_scope() == WorkspaceScope::Local,
        "fixture scope changed"
    );
    ensure!(
        core.links().is_none() && core.updater().is_none(),
        "fixture gained an Edge path"
    );
    seed_mock_task(root, &core)?;
    std::fs::write(root.join(PREPARED), PREPARED_CONTENTS)?;
    Ok(Arc::new(core))
}

/// Seed one navigable idle task through the normal local workspace registry.
/// The QA fixture never starts a provider or fabricates a completed turn.
fn seed_mock_task(root: &Path, core: &EngineCore) -> Result<()> {
    let project_path = root.join("synthetic-project");
    std::fs::create_dir_all(&project_path)?;
    let project_path = project_path.to_string_lossy().into_owned();
    let space_id = match core
        .workspace
        .read_spaces()?
        .into_iter()
        .find(|space| space.device_id == core.device_id && space.path == project_path)
    {
        Some(space) => space.id,
        None => {
            core.workspace.create_space(
                QA_PROJECT_ID,
                &core.device_id,
                &project_path,
                Some("L32 synthetic project".into()),
                root.join("synthetic-project/.git").is_dir(),
            )?;
            QA_PROJECT_ID.into()
        }
    };
    if core.workspace.chat(QA_TASK_ID)?.is_none() {
        core.workspace.create_chat(
            QA_TASK_ID,
            Some(&space_id),
            None,
            Some(ChatConfig {
                harness: HarnessId::Mock,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                permission_mode: Default::default(),
                effective_permission_mode: Default::default(),
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: Vec::new(),
            }),
            None,
        )?;
        core.workspace
            .rename_chat(QA_TASK_ID, "L32 synthetic mock task")?;
    }
    let chat = core
        .workspace
        .chat(QA_TASK_ID)?
        .context("QA task missing")?;
    ensure!(
        chat.space_id.as_deref() == Some(space_id.as_str())
            && chat
                .config
                .as_ref()
                .is_some_and(|config| config.harness == HarnessId::Mock),
        "QA task is not bound to the synthetic mock project"
    );
    if !core
        .workspace
        .read_sessions()?
        .iter()
        .any(|session| session.chat_id == QA_TASK_ID)
    {
        core.workspace.record_session(&Session {
            chat_id: QA_TASK_ID.into(),
            device_id: core.device_id.clone(),
            status: SessionStatus::Idle,
            started_at: None,
            updated_at: std::time::SystemTime::now().into(),
            last_completed_turn: None,
            recovery: None,
        });
    }
    ensure!(
        core.workspace
            .read_sessions()?
            .iter()
            .any(|session| session.chat_id == QA_TASK_ID && session.device_id == core.device_id),
        "QA session was not persisted"
    );
    Ok(())
}

/// `main` has already canonicalized and validated this root before logging.
pub fn run(root: PathBuf, check_only: bool) -> Result<()> {
    // Core assembly starts local maintenance tasks; retain this runtime for
    // the whole native app lifetime instead of merely entering it to build.
    let runtime = tokio::runtime::Runtime::new()?;
    let core = runtime.block_on(async { prepare_core(&root) })?;
    tracing::info!(root = %root.display(), "L32 synthetic native fixture prepared");
    if check_only {
        if std::fs::symlink_metadata(root.join(QA_BUNDLE)).is_ok() {
            let copied_executable = root.join(QA_BUNDLE).join("Contents/MacOS/luna");
            ensure!(
                qa_root_from_executable(&copied_executable)?.as_deref() == Some(root.as_path()),
                "copied QA bundle does not resolve this synthetic root"
            );
        }
        let result: Result<()> = runtime.block_on(async {
            let handle = luna_ui::EngineHandle::synthetic_from_core(core.clone()).await?;
            ensure!(
                handle.engine_info().workspace_scope == WorkspaceScope::Local,
                "fixture readback scope changed"
            );
            let catalog = handle
                .client()
                .call(luna_rpc::methods::LIST_HARNESSES, serde_json::json!({}))
                .await?;
            ensure!(
                catalog.as_array().is_some_and(|items| items.len() == 1),
                "fixture registry is not mock-only"
            );
            ensure!(
                core.workspace.chat(QA_TASK_ID)?.is_some(),
                "synthetic QA task is missing"
            );
            Ok(())
        });
        runtime.block_on(core.shutdown());
        result?;
        println!("L32 synthetic native fixture ready: local mock, Edge disabled");
        return Ok(());
    }
    luna_ui::run_app(luna_ui::UiConfig {
        data_dir: root.join("data"),
        ipc_port: 0,
        edge_url: luna_engine::OFFLINE_EDGE_URL.into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
        initial_url: None,
        synthetic_core: Some(core),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_executable(root: &Path, bundle: &str) -> PathBuf {
        let executable = root.join(bundle).join("Contents/MacOS/luna");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"synthetic Mach-O placeholder").unwrap();
        executable
    }

    #[test]
    fn mock_task_seed_is_idempotent_and_stays_in_fixture_root() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join(MARKER), MARKER_CONTENTS).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let core = runtime
            .block_on(async { prepare_core(fixture.path()) })
            .unwrap();
        seed_mock_task(fixture.path(), &core).unwrap();

        let project = core.workspace.read_spaces().unwrap();
        let chats = core.workspace.read_chats().unwrap();
        let sessions = core.workspace.read_sessions().unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(
            project[0].path,
            fixture.path().join("synthetic-project").to_string_lossy()
        );
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].id, QA_TASK_ID);
        assert_eq!(chats[0].config.as_ref().unwrap().harness, HarnessId::Mock);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].chat_id, QA_TASK_ID);
        assert_eq!(sessions[0].status, SessionStatus::Idle);
        assert!(sessions[0].last_completed_turn.is_none());
        runtime.block_on(core.shutdown());
    }

    #[test]
    fn marker_and_private_temp_root_are_required() {
        let fixture = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        assert!(validate_root(fixture.path()).is_err());
        std::fs::write(fixture.path().join(MARKER), MARKER_CONTENTS).unwrap();
        assert_eq!(
            validate_root(fixture.path()).unwrap(),
            fixture.path().canonicalize().unwrap()
        );
        std::fs::write(fixture.path().join("foreign-data"), b"synthetic refusal").unwrap();
        assert!(validate_root(fixture.path()).is_err());
        std::fs::remove_file(fixture.path().join("foreign-data")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/", fixture.path().join("escape")).unwrap();
            assert!(validate_root(fixture.path()).is_err());
        }
    }

    #[test]
    fn no_argument_route_requires_exact_prepared_qa_bundle() {
        let fixture = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        std::fs::write(fixture.path().join(MARKER), MARKER_CONTENTS).unwrap();
        std::fs::write(fixture.path().join(PREPARED), PREPARED_CONTENTS).unwrap();
        let normal = synthetic_executable(fixture.path(), "LUNA.app");
        assert!(qa_root_from_executable(&normal).unwrap().is_none());
        let qa = synthetic_executable(fixture.path(), QA_BUNDLE);
        assert_eq!(
            qa_root_from_executable(&qa).unwrap(),
            Some(fixture.path().canonicalize().unwrap())
        );
        std::fs::write(fixture.path().join(PREPARED), b"bad marker").unwrap();
        assert!(qa_root_from_executable(&qa).is_err());
        std::fs::remove_file(fixture.path().join(PREPARED)).unwrap();
        assert!(qa_root_from_executable(&qa).is_err());
        std::fs::write(fixture.path().join(PREPARED), PREPARED_CONTENTS).unwrap();
        std::fs::write(fixture.path().join(MARKER), b"bad marker").unwrap();
        assert!(qa_root_from_executable(&qa).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn qa_bundle_symlink_and_malformed_path_fail_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let fixture = tempfile::tempdir().unwrap();
        std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(fixture.path().join(MARKER), MARKER_CONTENTS).unwrap();
        std::fs::write(fixture.path().join(PREPARED), PREPARED_CONTENTS).unwrap();
        let normal = synthetic_executable(fixture.path(), "LUNA.app");
        symlink(
            fixture.path().join("LUNA.app"),
            fixture.path().join(QA_BUNDLE),
        )
        .unwrap();
        let linked = fixture.path().join(QA_BUNDLE).join("Contents/MacOS/luna");
        assert!(qa_root_from_executable(&linked).is_err());
        std::fs::remove_file(fixture.path().join(QA_BUNDLE)).unwrap();
        let malformed = fixture.path().join(QA_BUNDLE).join("Other/MacOS/luna");
        assert!(qa_root_from_executable(&malformed).is_err());
        assert!(qa_root_from_executable(&normal).unwrap().is_none());
    }
}
