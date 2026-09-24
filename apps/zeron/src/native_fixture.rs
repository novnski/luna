//! Opt-in local native QA for the shipped Mach-O, with no provider or Edge path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, WorkspaceScope,
    local_adoption_mock_registry,
};

const MARKER: &str = ".luna-l32-native-fixture";
const MARKER_CONTENTS: &[u8] = b"L32 synthetic native fixture\n";
const PREPARED: &str = ".luna-l32-prepared";
const PREPARED_CONTENTS: &[u8] = b"L32 prepared synthetic root v1\n";
const MAX_ENTRIES: usize = 10_000;

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
    let mut auth_config = AuthConfig::new(zeron_engine::OFFLINE_EDGE_URL, &data);
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
    std::fs::write(root.join(PREPARED), PREPARED_CONTENTS)?;
    Ok(Arc::new(core))
}

/// `main` has already canonicalized and validated this root before logging.
pub fn run(root: PathBuf, check_only: bool) -> Result<()> {
    // Core assembly starts local maintenance tasks; retain this runtime for
    // the whole native app lifetime instead of merely entering it to build.
    let runtime = tokio::runtime::Runtime::new()?;
    let core = runtime.block_on(async { prepare_core(&root) })?;
    tracing::info!(root = %root.display(), "L32 synthetic native fixture prepared");
    if check_only {
        let result: Result<()> = runtime.block_on(async {
            let handle = zeron_ui::EngineHandle::synthetic_from_core(core.clone()).await?;
            ensure!(
                handle.engine_info().workspace_scope == WorkspaceScope::Local,
                "fixture readback scope changed"
            );
            let catalog = handle
                .client()
                .call(zeron_rpc::methods::LIST_HARNESSES, serde_json::json!({}))
                .await?;
            ensure!(
                catalog.as_array().is_some_and(|items| items.len() == 1),
                "fixture registry is not mock-only"
            );
            Ok(())
        });
        runtime.block_on(core.shutdown());
        result?;
        println!("L32 synthetic native fixture ready: local mock, Edge disabled");
        return Ok(());
    }
    zeron_ui::run_app(zeron_ui::UiConfig {
        data_dir: root.join("data"),
        ipc_port: 0,
        edge_url: zeron_engine::OFFLINE_EDGE_URL.into(),
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
}
