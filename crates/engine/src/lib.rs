//! luna-engine — the headless backend: sessions engine, doc host + command executor,
//! run journal + crash recovery, and the IPC RPC server.
//!
//! Spec: ARCHITECTURE.md §5 and docs/research/feature-inventory.md §3. M2 surface:
//! sessions + docs + commands + minimal IPC. Terminals, repos/diffs, uploads, auth,
//! agent accounts, and the device-room host land in later milestones.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
pub use luna_proto::{EngineInfo, HarnessId, WorkspaceScope};
use luna_rpc::{RpcError, RpcReply, RpcService, methods};
use sha2::{Digest, Sha256};

use luna_sync::DocsStore;

/// No owned sync service has been configured for a local LUNA installation.
pub const OFFLINE_EDGE_URL: &str = "luna-local://offline";
/// Distinct from Luna's historical owner IPC port.
pub const DEFAULT_LUNA_IPC_PORT: u16 = 59778;

/// Parse an explicitly configured service without accepting placeholder hosts.
/// `Url` canonicalizes case and IDNA; a trailing DNS root dot is equivalent
/// for the placeholder-domain check. Credentials in URLs are refused entirely.
pub fn canonical_owned_edge_url(raw: &str) -> anyhow::Result<String> {
    let parsed = url::Url::parse(raw.trim())?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https") && parsed.has_host(),
        "LUNA_EDGE_URL requires an HTTP(S) URL with a host"
    );
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "LUNA_EDGE_URL must not contain credentials"
    );
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        let host = domain.trim_end_matches('.');
        anyhow::ensure!(
            host != "luna.invalid" && !host.ends_with(".luna.invalid"),
            "LUNA cannot use a placeholder service endpoint"
        );
    }
    Ok(parsed.to_string())
}

/// Stable, path-free claim for one canonical local data root. Only the
/// loopback owner IPC wrapper returns this value to a client.
pub fn local_root_claim(root: &Path) -> std::io::Result<String> {
    let canonical = root.canonicalize()?;
    if !canonical.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "LUNA data root is not a directory",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"LUNA local root v1\0");
    hash.update(canonical.as_os_str().as_encoded_bytes());
    Ok(format!("{:x}", hash.finalize()))
}

struct LocalOwnerRpc {
    inner: Arc<dyn RpcService>,
    claim: String,
}

#[async_trait]
impl RpcService for LocalOwnerRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == methods::LOCAL_OWNER_ROOT_CLAIM {
            return RpcReply::value(&serde_json::json!({ "claim": self.claim }));
        }
        self.inner.handle(method, params).await
    }
}

/// Add the root proof to a local loopback IPC service only. The remote RPC
/// factory keeps the unwrapped service and cannot answer this method.
pub fn local_owner_ipc_service(
    inner: Arc<dyn RpcService>,
    root: &Path,
) -> std::io::Result<Arc<dyn RpcService>> {
    Ok(Arc::new(LocalOwnerRpc {
        inner,
        claim: local_root_claim(root)?,
    }))
}

#[cfg(test)]
mod local_adoption_url_tests {
    use super::canonical_owned_edge_url;

    #[test]
    fn canonical_host_guard_rejects_vendor_aliases_and_credentials() {
        for value in [
            "https://edge.luna.invalid.",
            "HTTPS://EDGE.luna.invalid./releases",
            "https://user:password@owned.example",
            "https://",
        ] {
            assert!(canonical_owned_edge_url(value).is_err(), "{value}");
        }
        assert_eq!(
            canonical_owned_edge_url("HTTP://[::1]:7777").unwrap(),
            "http://[::1]:7777/"
        );
        assert_eq!(
            canonical_owned_edge_url("https://owned.example/api").unwrap(),
            "https://owned.example/api"
        );
    }
}

mod activity_history;
pub mod agent_accounts;
mod attention;
pub mod auth;
pub mod backup;
pub mod backup_control;
pub mod change_requests;
pub mod chat2_host;
pub mod context_controls;
pub mod credentials;
pub mod cross_task_tools;
mod device_write;
pub mod diff_sync;
pub mod doc_host;
pub mod git_actions;
pub mod git_checkpoints;
pub mod history_operations;
mod http_error;
pub mod instance_lock;
// The owner-managed action seam is internal until an owner driver is verified.
mod composer_catalog;
#[allow(dead_code)]
pub(crate) mod integration_actions;
pub mod integration_settings;
pub mod local_import;
#[cfg(target_os = "macos")]
mod macos_keychain;
mod model_cache;
pub mod permissions;
pub mod profile;
pub mod project_commands;
mod provider_updates;
pub mod recovery;
pub mod registry;
pub mod repos;
pub mod rpc;
pub mod run_journal;
pub mod runtime_contract;
pub mod sessions;
pub mod side_chats;
pub mod source_control;
pub mod spaces;
pub mod task_identity;
pub mod terminals;
pub mod titles;
mod transcript_history;
pub mod uploads;
pub mod usage;
mod usage_opencode;
pub mod workspace_files;
pub mod workspace_host;

pub use agent_accounts::{
    AgentAccounts, AgentAccountsConfig, CLAUDE_NATIVE_ACCOUNT_ID, CODEX_NATIVE_ACCOUNT_ID,
};
pub use auth::{Auth, AuthConfig, AuthState, AuthUser, OrgMembership};
pub use change_requests::{ChangeRequestCacheKey, CheckoutChangeRequests};
pub use credentials::{
    CredentialBroker, CredentialClass, CredentialError, CredentialLease, CredentialMetadata,
    CredentialScope, CredentialState, SecretIngress,
};
pub use diff_sync::{
    CheckoutDiffSync, DiffFileTextPair, DiffSidecar, DiffSnapshot, TurnSnapshot,
    capture_commit_diff, capture_diff, capture_diff_against, capture_turn_diff, merge_base,
    read_diff_file_text, snapshot_tree, working_diff_base,
};
pub use doc_host::{ChatDocHandle, DocHost, DocHostConfig, EdgeConfig};
pub use git_checkpoints::GitCheckpoints;
pub use instance_lock::InstanceLock;
pub use permissions::{provider_capabilities, provider_capabilities_for, resolve_run_policy};
pub use profile::EngineProfile;
pub use recovery::{RecoveryDecision, classify_failure, recovery_decision};
pub use registry::{
    HarnessDescriptor, HarnessRegistry, default_registry, local_adoption_mock_registry,
};
pub use repos::{CheckoutIdentity, Repos, worktree_branch_from_title};
pub use rpc::EngineRpc;
pub use run_journal::{JournalError, RunJournal};
pub use sessions::{JournaledEvent, SessionsEngine, SteerOutcome};
pub use source_control::{
    BranchHeadContext, ChangeRequestError, ChangeRequestProvider, ChangeRequestResolution,
    ChangeRequestResolver, CheckoutChangeRequestLookup, CheckoutSourceContext, GitHubCli,
    GitRemote, parse_git_remote,
};
pub use spaces::SpacesSync;
pub use task_identity::TaskIdentityService;
pub use terminals::Terminals;
pub use titles::TitleGenerator;
pub use uploads::{AttachmentChunk, Uploads};
pub use workspace_files::WorkspaceFiles;
pub use workspace_host::{
    DEFAULT_ORG_ID, DEFAULT_USER_ID, WORKSPACE_DOC_ID, WorkspaceHost, WorkspaceHostConfig,
};

pub(crate) const LEGACY_UNKNOWN_DEVICE_NAME: &str = "unknown-device";

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("credential: {0}")]
    Credential(#[from] credentials::CredentialError),
    #[error(transparent)]
    Token(#[from] luna_rpc::TokenError),
    #[error("doc: {0}")]
    Doc(#[from] luna_doc::DocError),
    #[error("journal: {0}")]
    Journal(#[from] run_journal::JournalError),
    #[error("store: {0}")]
    Store(#[from] luna_sync::StoreError),
    #[error("harness: {0}")]
    Harness(#[from] luna_harness::HarnessError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

/// Epoch millis now — the doc/journal timestamp base.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

enum CodexMetadataAssembly {
    Native,
    Disabled,
    Injected(luna_harness::codex::native::CodexNativeClient),
}

#[derive(Clone)]
pub struct EngineConfig {
    /// Data directory (default `~/.luna`, development builds use an isolated target path).
    pub data_dir: PathBuf,
    /// Edge base URL.
    pub edge_url: String,
    /// Explicit development bearer for edge room joins. Synced WorkOS runtimes
    /// obtain their bearer from [`Auth`]; development stays offline when this is absent.
    pub edge_token: Option<String>,
    /// Localhost IPC port for the UI.
    pub ipc_port: u16,
    /// Harness for doc-command runs on chats without a workspace `config` row.
    pub default_harness: HarnessId,
    /// Workspace-doc org (`ws/{orgId}` room). `None` = `$LUNA_ORG_ID` or the dev default.
    /// In WorkOS mode the signed-in session's org wins.
    pub org_id: Option<String>,
    /// WorkOS client id — enables real auth; `None` = dev mode (bearer = `edge_token`).
    pub workos_client_id: Option<String>,
}

impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("data_dir", &self.data_dir)
            .field("edge_url", &self.edge_url)
            .field(
                "edge_token",
                &self.edge_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("ipc_port", &self.ipc_port)
            .field("default_harness", &self.default_harness)
            .field("org_id", &self.org_id)
            .field("workos_client_id", &self.workos_client_id)
            .finish()
    }
}

/// The assembled engine core — also constructible without the IPC server for tests
/// and the in-process (headed) mode.
pub struct EngineCore {
    github_pr: source_control::pull_requests::GitHubPullRequests,
    backup_control: Option<backup_control::BackupControl>,
    pub store: Arc<DocsStore>,
    pub sessions: SessionsEngine,
    pub side_chats: side_chats::SideChats,
    pub doc_host: DocHost,
    pub workspace: WorkspaceHost,
    pub registry: Arc<HarnessRegistry>,
    pub repos: Repos,
    pub workspace_files: WorkspaceFiles,
    pub terminals: Terminals,
    pub project_commands: project_commands::ProjectCommands,
    pub previews: luna_preview::PreviewService,
    pub change_requests: CheckoutChangeRequests,
    pub diff_sync: CheckoutDiffSync,
    pub spaces_sync: SpacesSync,
    pub uploads: Uploads,
    pub agent_accounts: AgentAccounts,
    model_cache: Arc<model_cache::ModelCache>,
    pub task_identity: TaskIdentityService,
    pub device_id: String,
    remote_auth: Option<Arc<luna_rpc::RemoteAuthStore>>,
    /// Local→synced profile import (account-scoped runtimes only).
    pub local_import: Option<local_import::LocalImporter>,
    workspace_scope: WorkspaceScope,
    /// Auth service (attached by [`Engine::run`]; a lazy dev-mode instance otherwise).
    auth: std::sync::Mutex<Option<Auth>>,
    /// Peer link cache for `targetDeviceId` routing (attached when edge+auth are ready).
    links: std::sync::Mutex<Option<Arc<luna_rpc::LinkCache>>>,
    /// Release checker (attached by [`Engine::assemble_runtime`]) — the
    /// UpdateStatus stream + ApplyUpdate.
    updater: std::sync::Mutex<Option<luna_update::Updater>>,
    /// The updater's token-change wake forwarder — owned so shutdown can end it.
    updater_wake: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Owns the exclusive data-dir lock and retires future write admission.
    device_write: device_write::DeviceWriteOwner,
    /// Shared by every RPC service in this engine; changes on a real reopen.
    cleanup_boot_id: String,
    cleanup_cancel: tokio_util::sync::CancellationToken,
    cleanup_worker: std::sync::Mutex<Option<(Arc<rpc::EngineRpc>, tokio::task::JoinHandle<()>)>>,
}

impl EngineCore {
    /// Open stores under `data_dir`, wire sessions ⇄ doc host ⇄ workspace host, and
    /// recover stale journals from a previous crash. Identity comes from
    /// `$LUNA_ORG_ID` / `$LUNA_USER_ID` (dev defaults `dev-org` / `dev-user`);
    /// use [`Self::assemble_with_identity`] to pass one explicitly.
    pub fn assemble(
        data_dir: &Path,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
    ) -> Result<Self, EngineError> {
        let org_id = env_or("LUNA_ORG_ID", DEFAULT_ORG_ID);
        let user_id = env_or("LUNA_USER_ID", DEFAULT_USER_ID);
        let profile = EngineProfile::development(data_dir, &org_id, &user_id);
        Self::assemble_with_profile(profile, registry, default_harness, edge)
    }

    pub fn assemble_with_identity(
        data_dir: &Path,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        org_id: &str,
        user_id: &str,
    ) -> Result<Self, EngineError> {
        let profile = EngineProfile::synced(data_dir, org_id, user_id);
        Self::assemble_with_profile(profile, registry, default_harness, edge)
    }

    /// Assemble the engine against one resolved, immutable workspace profile.
    pub fn assemble_with_profile(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        // Single-instance guard: two engines on one data dir would race the
        // SQLite snapshots + journals. Taken before any store opens or the IPC
        // port binds; held (and kernel-released on crash) for the engine's life.
        let lock = InstanceLock::acquire(data_dir)?;
        Self::assemble_with_profile_locked(profile, registry, default_harness, edge, lock)
    }

    /// Assemble against a pre-acquired [`InstanceLock`]. The headed app takes
    /// the lock before binding the IPC port so the listener owner and the
    /// data-dir owner cannot diverge when several viewports bootstrap at once.
    pub fn assemble_with_profile_locked(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        lock: InstanceLock,
    ) -> Result<Self, EngineError> {
        Self::assemble_with_profile_locked_and_accounts(
            profile,
            registry,
            default_harness,
            edge,
            lock,
            None,
            CodexMetadataAssembly::Native,
        )
    }

    /// Hermetic assembly seam for tests and native UI fixtures. Passing an
    /// explicit provider-account config prevents any fallback to the user's
    /// real provider homes.
    #[doc(hidden)]
    pub fn assemble_with_profile_and_agent_accounts_config(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        agent_accounts_config: AgentAccountsConfig,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        let lock = InstanceLock::acquire(data_dir)?;
        Self::assemble_with_profile_locked_and_accounts(
            profile,
            registry,
            default_harness,
            edge,
            lock,
            Some(agent_accounts_config),
            CodexMetadataAssembly::Native,
        )
    }

    /// Hermetic fixture/test assembly. Codex-shaped in-process harnesses can
    /// exercise production identity and lifecycle reducers, but native Codex
    /// metadata discovery/mutation is disabled and cannot resolve an installed
    /// executable.
    #[doc(hidden)]
    pub fn assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        agent_accounts_config: AgentAccountsConfig,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        let lock = InstanceLock::acquire(data_dir)?;
        Self::assemble_with_profile_locked_and_accounts(
            profile,
            registry,
            default_harness,
            edge,
            lock,
            Some(agent_accounts_config),
            CodexMetadataAssembly::Disabled,
        )
    }

    /// Hermetic fixture/test assembly with an explicit native-metadata client.
    /// The caller owns the executable and provider home; no installed Codex
    /// discovery or credential fallback is available through this seam.
    #[doc(hidden)]
    pub fn assemble_with_profile_and_injected_codex_metadata_client(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        agent_accounts_config: AgentAccountsConfig,
        codex: luna_harness::codex::native::CodexNativeClient,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        let lock = InstanceLock::acquire(data_dir)?;
        Self::assemble_with_profile_locked_and_accounts(
            profile,
            registry,
            default_harness,
            edge,
            lock,
            Some(agent_accounts_config),
            CodexMetadataAssembly::Injected(codex),
        )
    }

    fn assemble_with_profile_locked_and_accounts(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        lock: InstanceLock,
        agent_accounts_config: Option<AgentAccountsConfig>,
        codex_metadata: CodexMetadataAssembly,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        backup::ensure_no_pending_managed_restore(data_dir)
            .map_err(|error| EngineError::Other(error.to_string()))?;
        std::fs::create_dir_all(data_dir)?;
        let device_write = device_write::DeviceWriteOwner::new(data_dir, lock)?;
        let legacy_uploads_root = profile.claim_legacy_uploads_root()?;
        let device_id = load_or_create_device_id(data_dir)?;
        let environment_id = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &device_id,
                profile.scope(),
                profile.org_id(),
                profile.user_id(),
            ))?)
        );
        let remote_auth = match luna_rpc::RemoteAuthStore::open(
            profile.store_root(),
            luna_rpc::RemoteEnvironment {
                device_id: device_id.clone(),
                workspace_scope: profile.scope(),
                environment_id,
            },
        ) {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::error!(%error, "remote authority disabled; local engine remains available");
                None
            }
        };
        // This device's harness enablement (Settings → Agents) rides the
        // engine data dir — per-device, like the CLI installs it gates.
        registry
            .load_prefs_guarded(data_dir, device_write.handle())
            .map_err(|error| EngineError::Other(error.to_string()))?;
        let store = Arc::new(DocsStore::open(profile.store_root())?);
        let store_for_import = store.clone();
        let journal = Arc::new(RunJournal::open_guarded(
            profile.store_root().join("journals"),
            device_write.handle(),
        )?);
        let sessions =
            SessionsEngine::new(device_id.clone(), journal, registry.clone(), store.clone());
        sessions.set_recovery_settings_path(profile.device_root().join("ui-settings.json"));
        sessions.set_integration_settings(integration_settings::IntegrationSettings::open(
            profile.store_root().join("luna-instructions.json"),
        )?);
        let doc_host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: device_id.clone(),
                default_harness,
                edge: edge.clone(),
            },
        );
        let workspace = WorkspaceHost::open_with_registry_write_handle(
            store.clone(),
            WorkspaceHostConfig {
                device_id: device_id.clone(),
                device_name: local_device_name(&device_id),
                platform: std::env::consts::OS.to_string(),
                org_id: profile.org_id().to_string(),
                user_id: profile.user_id().to_string(),
                edge: edge.clone(),
            },
            device_write.handle(),
        )?;
        let agent_accounts_config =
            agent_accounts_config.unwrap_or_else(|| AgentAccountsConfig::detect(data_dir));
        let task_identity = match codex_metadata {
            CodexMetadataAssembly::Native => TaskIdentityService::open(
                store.clone(),
                workspace.clone(),
                &device_id,
                agent_accounts_config.codex_home.clone(),
            )?,
            CodexMetadataAssembly::Disabled => TaskIdentityService::open_without_codex_metadata(
                store.clone(),
                workspace.clone(),
                &device_id,
                agent_accounts_config.codex_home.clone(),
            )?,
            CodexMetadataAssembly::Injected(codex) => TaskIdentityService::open_with_codex_client(
                store.clone(),
                workspace.clone(),
                &device_id,
                codex,
            )?,
        };
        task_identity.install_write_handle(device_write.handle())?;
        doc_host.set_workspace(workspace.clone());
        doc_host.set_sessions(sessions.clone());
        sessions.set_doc_host(doc_host.clone());
        sessions.set_task_identity(task_identity.clone());
        let side_chats = side_chats::SideChats::open(
            &profile.store_root(),
            device_write.handle(),
            sessions.clone(),
            doc_host.clone(),
            workspace.clone(),
        )
        .map_err(|error| EngineError::Other(error.to_string()))?;
        match sessions.recover_stale() {
            Ok(0) => {}
            Ok(recovered) => tracing::info!(recovered, "stale sessions recovered on boot"),
            Err(err) => tracing::error!(error = %err, "stale-session recovery failed"),
        }
        doc_host.spawn_transcript_salvage(profile.store_root().join("journals"));
        let repos = Repos::new_guarded(data_dir, &device_id, device_write.handle());
        doc_host.set_repos(repos.clone());
        let change_requests = CheckoutChangeRequests::start(repos.clone(), &device_id);
        let workspace_files =
            WorkspaceFiles::new(repos.clone(), workspace.clone(), device_id.clone());
        let terminals =
            Terminals::open_with_history(&profile.store_root().join("terminals"), &device_id)?;
        let project_commands = project_commands::ProjectCommands::open(
            store.clone(),
            workspace.clone(),
            terminals.clone(),
            repos.clone(),
            device_id.clone(),
            device_write.handle(),
        )?;
        doc_host.set_project_commands(project_commands.clone());
        let previews = luna_preview::PreviewService::new(
            profile.store_root().join("previews.json"),
            device_id.clone(),
            local_device_name(&device_id),
        )
        .map_err(|e| EngineError::Other(e.to_string()))?;
        let uploads = Uploads::from_root_with_fallback_guarded(
            profile.uploads_root(),
            legacy_uploads_root.as_deref(),
            device_write.handle(),
        );
        // A recorded local→synced import grants this account the local
        // profile's uploads root read-only — transcripts imported earlier
        // embed absolute paths under it (same shape as the legacy adoption).
        if profile.scope() != WorkspaceScope::Local
            && let Some(root) =
                local_import::marker_grants_read_root(data_dir, profile.org_id(), profile.user_id())
        {
            uploads.add_read_only_root(&root);
        }
        // Queued-attachment support: the doc host resolves `pending://` refs
        // against this store and pushes staged bytes to remote hosts.
        doc_host.set_uploads(uploads.clone());
        sessions.set_generated_images(
            uploads.clone(),
            agent_accounts_config.codex_home.join("generated_images"),
        );
        let local_import = (profile.scope() == WorkspaceScope::Synced).then(|| {
            local_import::LocalImporter::new_guarded(
                data_dir,
                &device_id,
                profile.org_id(),
                profile.user_id(),
                store_for_import.clone(),
                profile.store_root().join("journals"),
                workspace.clone(),
                uploads.clone(),
                device_write.handle(),
            )
        });
        let agent_accounts = AgentAccounts::new(agent_accounts_config);
        sessions.set_agent_accounts(agent_accounts.clone());
        let titles = TitleGenerator::new(workspace.clone(), registry.clone(), repos.clone());
        titles.set_task_identity(task_identity.clone());
        sessions.set_titles(titles);
        let diff_sync = CheckoutDiffSync::start(repos.clone(), workspace.clone(), &device_id, edge);
        let checkpoints = GitCheckpoints::new(repos.clone());
        let github_pr = source_control::pull_requests::GitHubPullRequests::new(
            checkpoints.actions(),
            profile.store_root().join("github-pr-attempts"),
        )
        .guarded(device_write.handle());
        sessions.set_git_checkpoints(checkpoints);
        // Turn starts snapshot the checkout tree — the "Latest turn" diff base.
        let turn_diff = diff_sync.clone();
        sessions.set_turn_listener(Arc::new(move |chat_id, cwd| {
            turn_diff.note_turn_start(chat_id, cwd);
        }));
        let spaces_sync = SpacesSync::start(repos.clone(), workspace.clone(), &device_id);
        task_identity.start_startup_reconciliation();
        let backup_control = backup::IsolatedRoot::open(data_dir)
            .ok()
            .map(backup_control::BackupControl::new);
        let model_cache = Arc::new(model_cache::ModelCache::new(
            data_dir.to_path_buf(),
            device_write.handle(),
            registry.clone(),
            agent_accounts.clone(),
        ));
        Ok(Self {
            github_pr,
            side_chats,
            backup_control,
            store,
            sessions,
            doc_host,
            workspace,
            registry,
            repos,
            workspace_files,
            terminals,
            project_commands,
            previews,
            change_requests,
            diff_sync,
            spaces_sync,
            uploads,
            agent_accounts,
            model_cache,
            task_identity,
            device_id,
            remote_auth,
            local_import,
            workspace_scope: profile.scope(),
            auth: std::sync::Mutex::new(None),
            links: std::sync::Mutex::new(None),
            updater: std::sync::Mutex::new(None),
            updater_wake: std::sync::Mutex::new(None),
            device_write,
            cleanup_boot_id: uuid::Uuid::new_v4().to_string(),
            cleanup_cancel: tokio_util::sync::CancellationToken::new(),
            cleanup_worker: std::sync::Mutex::new(None),
        })
    }

    pub fn workspace_scope(&self) -> WorkspaceScope {
        self.workspace_scope
    }

    #[doc(hidden)]
    pub fn cleanup_boot_id_for_tests(&self) -> &str {
        &self.cleanup_boot_id
    }

    #[doc(hidden)]
    pub fn cleanup_worker_count_for_tests(&self) -> usize {
        usize::from(
            self.cleanup_worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some(),
        )
    }

    /// Hermetic RPC fixture seam: replace the dormant default broker with an
    /// explicitly in-memory one before exposing any backup method.
    #[doc(hidden)]
    pub fn with_backup_control_for_tests(mut self, control: backup_control::BackupControl) -> Self {
        self.backup_control = Some(control);
        self
    }

    /// Attach the auth service (before building the RPC service / relays).
    pub fn set_auth(&self, auth: Auth) {
        *self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(auth);
    }

    /// The attached auth service, or a lazily-created dev-mode one (in-process embeds
    /// that never wired WorkOS still answer AuthStatus honestly).
    pub fn auth(&self) -> Auth {
        let mut slot = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.get_or_insert_with(|| {
            let dev_user = std::env::var("LUNA_EDGE_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "dev-user".into());
            let mut config = AuthConfig::new("http://localhost:27640", std::env::temp_dir());
            config.dev_user_id = dev_user;
            Auth::new(config)
        })
        .clone()
    }

    /// Attach the peer link cache — enables `targetDeviceId` routing,
    /// [`Self::dial_device`], and the doc host's queued-attachment transfers.
    pub fn set_links(&self, links: Arc<luna_rpc::LinkCache>) {
        self.doc_host.set_links(links.clone());
        *self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(links);
    }

    pub fn links(&self) -> Option<Arc<luna_rpc::LinkCache>> {
        self.links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Attach the release checker (before building the RPC service).
    pub fn set_updater_wake(&self, handle: tokio::task::JoinHandle<()>) {
        *self
            .updater_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
    }

    pub fn set_updater(&self, updater: luna_update::Updater) {
        *self
            .updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(updater);
    }

    pub fn updater(&self) -> Option<luna_update::Updater> {
        self.updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A live RPC client to another device's engine through its relay DO (the router's
    /// dial seam). Cached per device; invalidated + re-dialed on failure.
    pub async fn dial_device(
        &self,
        device_id: &str,
    ) -> Result<Arc<luna_rpc::RpcClient>, EngineError> {
        let links = self
            .links()
            .ok_or_else(|| EngineError::Other("peer links unavailable (offline)".into()))?;
        links
            .client(device_id)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))
    }

    /// Start hosting our device room with per-client authenticated RPC dispatch.
    /// Legacy unauthenticated relay nudges are ignored by the transport.
    /// The token source re-reads edge auth on every (re)dial.
    pub fn start_host_relay(&self, edge_url: &str) -> Option<luna_rpc::HostRelay> {
        let remote_auth = self.remote_auth.clone()?;
        let auth = self.auth();
        let config =
            luna_rpc::HostRelayConfig::new(edge_url, self.device_id.clone(), Arc::new(auth));
        Some(luna_rpc::HostRelay::spawn(
            config,
            self.rpc_service_without_backup(),
            remote_auth,
        ))
    }

    pub fn rpc_service(&self) -> Arc<EngineRpc> {
        self.rpc_service_with_local_backup(true)
    }

    /// One authenticated remote connection, using the same guard installed
    /// for each device-room virtual connection. Never share this instance
    /// between callers: authentication belongs to a connection.
    pub fn remote_rpc_service(&self) -> Option<Arc<luna_rpc::remote_auth::RemoteRpcService>> {
        Some(Arc::new(luna_rpc::remote_auth::RemoteRpcService::new(
            self.rpc_service_without_backup(),
            self.remote_auth.clone()?,
        )))
    }

    /// The direct listener constructs one authorization guard per socket. The
    /// underlying engine service is shared, but no connection's session is.
    pub fn remote_rpc_factory(&self) -> Option<Arc<dyn Fn() -> Arc<dyn RpcService> + Send + Sync>> {
        let inner: Arc<dyn RpcService> = self.rpc_service_without_backup();
        let auth = self.remote_auth.clone()?;
        Some(Arc::new(move || {
            Arc::new(luna_rpc::remote_auth::RemoteRpcService::new(
                inner.clone(),
                auth.clone(),
            ))
        }))
    }

    /// Relay RPCs never expose backup configuration, key ingress, or archive
    /// paths. Only the in-process/localhost service receives the control.
    fn rpc_service_without_backup(&self) -> Arc<EngineRpc> {
        self.rpc_service_with_local_backup(false)
    }

    fn rpc_service_with_local_backup(&self, include_backup: bool) -> Arc<EngineRpc> {
        let mut rpc = EngineRpc::new(
            self.store.clone(),
            self.sessions.clone(),
            self.doc_host.clone(),
            self.workspace.clone(),
            self.registry.clone(),
            self.repos.clone(),
            self.workspace_files.clone(),
            self.terminals.clone(),
            self.change_requests.clone(),
            self.diff_sync.clone(),
            self.uploads.clone(),
            self.agent_accounts.clone(),
            self.model_cache.clone(),
            self.task_identity.clone(),
            self.workspace_scope,
            self.cleanup_boot_id.clone(),
        )
        .with_auth(self.auth())
        .with_previews(self.previews.clone())
        .with_github_pr(self.github_pr.clone())
        .with_side_chats(self.side_chats.clone());
        if include_backup {
            // Saved shell commands are a local desktop workflow, never relay RPCs.
            rpc = rpc.with_project_commands(self.project_commands.clone());
        }
        if let Some(remote_auth) = &self.remote_auth {
            rpc = rpc.with_remote_auth(remote_auth.clone());
        }
        if let Some(control) = self.backup_control.clone().filter(|_| include_backup) {
            rpc = rpc.with_backup_control(control);
        } else if include_backup {
            rpc = rpc.with_managed_backup_root(self.device_write.root().to_path_buf());
        }
        if let Some(links) = self.links() {
            rpc = rpc.with_links(links);
        }
        if let Some(updater) = self.updater() {
            rpc = rpc.with_updater(updater);
        }
        if let Some(importer) = self.local_import.clone() {
            rpc = rpc.with_local_import(importer);
        }
        let rpc = Arc::new(rpc);
        if tokio::runtime::Handle::try_current().is_ok() {
            let mut worker = self
                .cleanup_worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if worker.is_none() {
                let (task_after, project_after) = rpc.resume_cleanup_initial();
                let handle = rpc.start_cleanup_maintenance(
                    self.cleanup_cancel.clone(),
                    task_after,
                    project_after,
                );
                *worker = Some((rpc.clone(), handle));
            }
        }
        let backend: Arc<dyn cross_task_tools::CrossTaskBackend> = rpc.clone();
        self.sessions
            .set_cross_task_backend(Arc::downgrade(&backend));
        rpc
    }

    /// Revoke every account-scoped transport before any slower graceful
    /// draining. Connected sockets remain authorized by their handshake, so
    /// clearing credentials alone is not a security boundary.
    pub fn disconnect_edge(&self) {
        self.previews.stop();
        if let Some(links) = self.links() {
            links.disconnect_all();
        }
        self.doc_host.disconnect_edge();
        self.workspace.disconnect_edge();
    }

    /// Graceful teardown: settle live runs (streaming entries stamped `aborted`),
    /// kill live PTYs, stamp our workspace `lastSeenAt`, and flush every open doc
    /// snapshot.
    pub async fn shutdown(&self) -> bool {
        self.model_cache.shutdown();
        self.cleanup_cancel.cancel();
        let cleanup = self
            .cleanup_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((_, task)) = cleanup {
            let _ = task.await;
        }
        self.previews.shutdown().await;
        // A run interruption transitions its chat to Idle, and Idle normally
        // releases the next queued row. Freeze first so quitting never starts
        // recovered work while the engine is being torn down.
        self.doc_host.pause_all_queues();
        let mut clean = self.side_chats.shutdown().await;
        clean &= self.sessions.shutdown().await;
        self.terminals.shutdown();
        self.agent_accounts.shutdown();
        self.change_requests.shutdown();
        // Cancel + await every worker that can reach Edge before flushing: a
        // replaced synced runtime must not keep polling releases or draining
        // the attachment outbox under the old identity after Local boots.
        let wake = self
            .updater_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(wake) = wake {
            wake.abort();
            let _ = wake.await;
        }
        let updater = self
            .updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(updater) = updater {
            updater.shutdown().await;
        }
        self.diff_sync.shutdown().await;
        self.workspace_files.shutdown().await;
        self.spaces_sync.shutdown().await;
        if let Err(error) = self.doc_host.shutdown_workers_checked().await {
            tracing::error!(%error, "doc host final snapshot flush failed during shutdown");
            clean = false;
        }
        if let Err(error) = self.workspace.shutdown_checked().await {
            tracing::error!(%error, "workspace final snapshot or task drain failed during shutdown");
            clean = false;
        }
        // Break the sessions ⇄ doc-host retain cycle so the replaced graph can
        // actually be freed once the last handle drops.
        self.sessions.clear_doc_host();
        self.device_write.retire();
        clean
    }
}

impl Drop for EngineCore {
    fn drop(&mut self) {
        self.model_cache.shutdown();
        self.cleanup_cancel.cancel();
        if let Some((_, task)) = self
            .cleanup_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        self.device_write.retire();
    }
}

pub struct Engine {
    pub config: EngineConfig,
}

/// A fully assembled identity-scoped engine plus the relay handle whose lifetime
/// keeps this device reachable. Used by both the headless server and the headed
/// in-process engine so their production authentication paths cannot diverge.
pub struct EngineRuntime {
    core: EngineCore,
    host_relay: std::sync::Mutex<Option<luna_rpc::HostRelay>>,
}

/// IPC-only lifecycle control owned by `luna headless`. The regular
/// [`EngineRpc`] deliberately does not expose this method, so a viewport
/// attached to another headed process cannot shut down that process's engine.
struct HeadlessRpc {
    inner: Arc<dyn RpcService>,
    stop_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

#[async_trait]
impl RpcService for HeadlessRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method != methods::STOP_ENGINE {
            return self.inner.handle(method, params).await;
        }

        let stop_tx = self.stop_tx.clone();
        // Let the unary success frame reach the client before `Engine::run`
        // aborts the IPC server and drains the runtime.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = stop_tx.send(());
        });
        RpcReply::value(&serde_json::json!({ "ok": true }))
    }
}

impl EngineRuntime {
    pub fn core(&self) -> &EngineCore {
        &self.core
    }

    pub fn workspace_scope(&self) -> WorkspaceScope {
        self.core.workspace_scope()
    }

    pub fn disconnect_edge(&self) {
        // Revoke remote reachability before graceful draining. Sessions may
        // need time to settle; no authenticated relay RPC may enter during
        // that window after sign-out.
        self.host_relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.core.disconnect_edge();
    }

    pub async fn shutdown(&self) -> bool {
        self.disconnect_edge();
        self.core.shutdown().await
    }
}

impl Drop for EngineRuntime {
    fn drop(&mut self) {
        self.host_relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    /// Resolve the shared dev/WorkOS auth configuration for headed and headless
    /// modes. A clean WorkOS boot deliberately avoids probing Edge: signed-out
    /// installations must be able to start locally without network access.
    pub async fn build_auth(config: &EngineConfig) -> Auth {
        let mut auth_config = AuthConfig::new(config.edge_url.clone(), config.data_dir.clone());
        auth_config.offline_only = config.edge_url == OFFLINE_EDGE_URL;
        auth_config.workos_client_id = config.workos_client_id.clone();
        if let Ok(base) = std::env::var("LUNA_WORKOS_API_BASE")
            && !base.trim().is_empty()
        {
            auth_config.workos_api_base = base;
        }
        auth_config.callback_port = Some(
            std::env::var("LUNA_CALLBACK_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(27641),
        );
        if let Some(token) = &config.edge_token {
            auth_config.dev_user_id = token.clone();
        }
        Auth::new(auth_config)
    }

    /// Capture the workspace boundary once, before refresh or sign-in can mutate auth.
    pub fn initial_workspace_scope(auth: &Auth) -> WorkspaceScope {
        if auth.offline_only() {
            WorkspaceScope::Local
        } else if !auth.workos_enabled() {
            WorkspaceScope::Development
        } else if auth.loaded_workos_session() {
            WorkspaceScope::Synced
        } else {
            WorkspaceScope::Local
        }
    }

    /// Resolve a profile for the captured scope. A synced session without an
    /// organization returns `None` until onboarding selects one; it never falls
    /// through to the local or development profile.
    pub fn resolve_profile(
        config: &EngineConfig,
        auth: &Auth,
        scope: WorkspaceScope,
    ) -> Result<Option<EngineProfile>, EngineError> {
        match scope {
            WorkspaceScope::Local => EngineProfile::local(&config.data_dir).map(Some),
            WorkspaceScope::Development => {
                let dev_token_org = config
                    .edge_token
                    .as_deref()
                    .and_then(|token| token.split_once('@'))
                    .map(|(_, org)| org.to_string())
                    .filter(|org| !org.is_empty());
                let org_id = dev_token_org
                    .or(config.org_id.clone())
                    .unwrap_or_else(|| env_or("LUNA_ORG_ID", DEFAULT_ORG_ID));
                let user_id = auth
                    .user_id()
                    .unwrap_or_else(|| env_or("LUNA_USER_ID", DEFAULT_USER_ID));
                Ok(Some(EngineProfile::development(
                    &config.data_dir,
                    &org_id,
                    &user_id,
                )))
            }
            WorkspaceScope::Synced => {
                let state = auth.state();
                let Some(user) = state.user() else {
                    return Err(EngineError::Other(
                        "captured synced session no longer exposes its user identity".into(),
                    ));
                };
                let Some(org_id) = state.org_id() else {
                    return Ok(None);
                };
                Ok(Some(EngineProfile::synced(
                    &config.data_dir,
                    org_id,
                    &user.id,
                )))
            }
        }
    }

    /// Resolve the one-shot identity served before profile stores are available.
    pub fn engine_info(
        config: &EngineConfig,
        workspace_scope: WorkspaceScope,
    ) -> Result<EngineInfo, EngineError> {
        std::fs::create_dir_all(&config.data_dir)?;
        Ok(EngineInfo {
            device_id: load_or_create_device_id(&config.data_dir)?,
            workspace_scope,
            capabilities: luna_proto::capabilities::current(),
        })
    }

    /// Open one already-resolved profile. Synced profiles always keep their
    /// Edge supervisors alive; temporary token or network failures are runtime
    /// states, not a reason to permanently assemble an offline engine.
    pub async fn assemble_runtime(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
    ) -> anyhow::Result<EngineRuntime> {
        Self::assemble_runtime_inner(config, auth, profile, None).await
    }

    /// Like [`Self::assemble_runtime`], but against an [`InstanceLock`] the
    /// caller already holds on the profile's device root (headed bootstrap
    /// acquires it before binding IPC).
    pub async fn assemble_runtime_with_lock(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
        lock: InstanceLock,
    ) -> anyhow::Result<EngineRuntime> {
        Self::assemble_runtime_inner(config, auth, profile, Some(lock)).await
    }

    async fn assemble_runtime_inner(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
        lock: Option<InstanceLock>,
    ) -> anyhow::Result<EngineRuntime> {
        let edge_enabled = match profile.scope() {
            WorkspaceScope::Local => false,
            WorkspaceScope::Synced => {
                // Validate the persisted session in the BACKGROUND: the probe
                // still transitions auth to SignedOut on definitive revocation
                // (and warms the single-flight refresh every first dial waits
                // on), but assembly — and the viewport blocked on it — no
                // longer stalls on a WorkOS round trip that can take seconds
                // on a bad link. Everything shown at boot is local anyway.
                let auth_probe = auth.clone();
                tokio::spawn(async move {
                    let _ = auth_probe.access_token().await;
                });
                true
            }
            // Dev Auth always exposes `dev_user_id` as its synthetic access
            // token, including when WorkOS was merely disabled with
            // LUNA_WORKOS_CLIENT_ID="". Only an explicitly configured,
            // non-empty bearer opts this runtime into Edge rooms and relays.
            WorkspaceScope::Development => config
                .edge_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty()),
        };
        if edge_enabled {
            // OS network-path events (macOS NWPathMonitor): the instant the
            // path returns every parked reconnect backoff redials, and while
            // the OS says there is no path the dial loops park instead of
            // burning attempts. No-op on platforms without a monitor.
            luna_sync::net_path::spawn_path_monitor();
        }
        let device_id = load_or_create_device_id(profile.device_root())?;
        let edge = edge_enabled.then(|| {
            EdgeConfig::new(config.edge_url.clone(), Arc::new(auth.clone())).with_device(device_id)
        });

        let preview_org = profile.org_id().to_string();
        let core = match lock {
            Some(lock) => EngineCore::assemble_with_profile_locked(
                profile,
                Arc::new(default_registry()),
                config.default_harness,
                edge.clone(),
                lock,
            )?,
            None => EngineCore::assemble_with_profile(
                profile,
                Arc::new(default_registry()),
                config.default_harness,
                edge.clone(),
            )?,
        };
        core.set_auth(auth.clone());
        let preview_workspace = core.workspace.clone();
        let preview_device = core.device_id.clone();
        let projects = Arc::new(move || {
            preview_workspace
                .read_chats()
                .unwrap_or_default()
                .into_iter()
                .filter(|chat| chat.device_id == preview_device)
                .filter_map(|chat| chat.cwd.map(std::path::PathBuf::from))
                .collect()
        });
        let preview_signaling = edge_enabled.then(|| luna_preview::signaling::Config {
            edge_url: config.edge_url.clone(),
            org_id: preview_org,
            tokens: Arc::new(auth.clone()),
        });
        core.previews.start(projects, preview_signaling).await;
        // Installed builds check the signed GitHub Releases feed even when the
        // engine is local-only and has no sync edge configured.
        let check_updates = !matches!(
            luna_update::detect_install(),
            luna_update::InstallKind::Unmanaged
        );
        if check_updates {
            // Release checker: polls GitHub Releases on a 6h cadence; headless
            // installs with LUNA_AUTO_UPDATE=1 apply + restart themselves — gated
            // on quiescence so a restart never lands under a live run or open PTY.
            let quiescent: luna_update::QuiescentCheck = {
                let sessions = core.sessions.clone();
                let terminals = core.terminals.clone();
                Arc::new(move || !sessions.any_active() && !terminals.any_open())
            };
            let updater = luna_update::Updater::spawn(config.edge_url.clone(), Some(quiescent));
            if let Some(mut token_changes) = edge.as_ref().and_then(EdgeConfig::token_changes) {
                let updater_for_tokens = updater.clone();
                let wake = tokio::spawn(async move {
                    while token_changes.changed().await.is_ok() {
                        updater_for_tokens.check_now();
                    }
                });
                core.set_updater_wake(wake);
            }
            core.set_updater(updater);
        }
        tracing::info!(device_id = %core.device_id, "engine core assembled");
        let host_relay = edge.as_ref().and_then(|edge| {
            let mut link_config =
                luna_rpc::LinkCacheConfig::new(edge.url.clone(), Arc::new(auth.clone()));
            // Registry-dark dial gate: devices with no recent presence fail
            // fast with zero dials; presence returning un-parks them (the
            // peer-alive hook below clears any cooldown at the same moment).
            let workspace_for_liveness = core.workspace.clone();
            link_config.liveness = Some(Arc::new(move |device_id: &str| {
                workspace_for_liveness.peer_liveness(device_id)
            }));
            let links = luna_rpc::LinkCache::new(link_config);
            let links_for_presence = links.clone();
            core.workspace
                .set_peer_alive_hook(Arc::new(move |device_id: &str| {
                    links_for_presence.reset_cooldown(device_id);
                }));
            core.set_links(links);
            core.start_host_relay(&edge.url)
        });

        Ok(EngineRuntime {
            core,
            host_relay: std::sync::Mutex::new(host_relay),
        })
    }

    /// Run until ctrl-c: auth (dev or WorkOS), sessions engine + doc host + command
    /// executor, IPC server, and — when edge+auth are ready — the device-room host
    /// relay + peer link cache (targetDeviceId routing).
    pub async fn run(self) -> anyhow::Result<()> {
        self.run_with_remote_port(None).await
    }

    /// Headless direct access is opt-in and binds only to remote loopback.
    /// SSH forwards this socket; local IPC keeps its separate owner authority.
    pub async fn run_with_remote_port(self, remote_port: Option<u16>) -> anyhow::Result<()> {
        let config = self.config;
        tracing::info!(data_dir = %config.data_dir.display(), "engine starting");

        std::fs::create_dir_all(&config.data_dir)?;
        let auth = Self::build_auth(&config).await;
        let auth_state = auth.watch_state();
        let workspace_scope = Self::initial_workspace_scope(&auth);
        let mut profile = Self::resolve_profile(&config, &auth, workspace_scope)?;
        let _refresh_loop = auth.spawn_refresh_loop();

        // A captured cloud session without an organization must finish onboarding
        // before its profile can open. A clean signed-out install is local and never
        // enters the terminal sign-in flow.
        if workspace_scope == WorkspaceScope::Synced && profile.is_none() {
            terminal_sign_in(&auth).await?;
            profile = Self::resolve_profile(&config, &auth, workspace_scope)?;
        }
        let profile = profile
            .ok_or_else(|| EngineError::Other("synced workspace profile is not ready".into()))?;

        let runtime = Self::assemble_runtime(&config, auth, profile).await?;

        Self::serve_headless_runtime(
            config,
            runtime,
            auth_state,
            workspace_scope,
            remote_port,
            true,
        )
        .await
    }

    /// Explicit hermetic binary fixture. The caller supplies isolated account
    /// paths; native provider metadata, Keychain fallback, Edge and preview
    /// supervisors are absent while the real headless listeners still run.
    #[doc(hidden)]
    pub async fn run_synthetic_with_remote_port(
        self,
        remote_port: u16,
        accounts: AgentAccountsConfig,
    ) -> anyhow::Result<()> {
        accounts.disable_claude_keychain_fallback()?;
        let config = self.config;
        if config.workos_client_id.is_some() || config.edge_token.is_some() {
            anyhow::bail!("synthetic headless startup requires offline auth");
        }
        let mut auth_config = AuthConfig::new(config.edge_url.clone(), config.data_dir.clone());
        auth_config.use_in_memory_credentials_for_tests();
        let auth = Auth::new(auth_config);
        let auth_state = auth.watch_state();
        let profile =
            EngineProfile::development(&config.data_dir, "l30-synthetic-org", "l30-synthetic-user");
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                profile,
                Arc::new(HarnessRegistry::new()),
                HarnessId::Mock,
                None,
                accounts,
            )?;
        core.set_auth(auth);
        let runtime = EngineRuntime {
            core,
            host_relay: std::sync::Mutex::new(None),
        };
        Self::serve_headless_runtime(
            config,
            runtime,
            auth_state,
            WorkspaceScope::Development,
            Some(remote_port),
            false,
        )
        .await
    }

    async fn serve_headless_runtime(
        config: EngineConfig,
        runtime: EngineRuntime,
        mut auth_state: tokio::sync::watch::Receiver<AuthState>,
        workspace_scope: WorkspaceScope,
        remote_port: Option<u16>,
        managed_backup: bool,
    ) -> anyhow::Result<()> {
        // A daemon exists to serve this port, so a bind failure is fatal here —
        // unlike the headed app, which can still work over its in-process
        // transport (see `serve_ipc`).
        let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel();
        let service: Arc<dyn RpcService> = Arc::new(HeadlessRpc {
            inner: runtime.core().rpc_service(),
            stop_tx,
        });
        let server = serve_ipc(config.ipc_port, service, &config.data_dir).await?;

        let remote_server = if let Some(port) = remote_port {
            if port == config.ipc_port {
                anyhow::bail!("remote port must differ from local IPC port");
            }
            let factory = runtime
                .core()
                .remote_rpc_factory()
                .ok_or_else(|| anyhow::anyhow!("remote authority unavailable"))?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
            tracing::info!(port, "authenticated remote RPC listening on loopback");
            Some(tokio::spawn(luna_rpc::serve_ws_listener_with_factory(
                listener, factory,
            )))
        } else {
            None
        };

        tokio::select! {
            result = shutdown_signal() => result?,
            requested = stop_rx.recv() => {
                if requested.is_some() {
                    tracing::info!("headless shutdown requested over IPC");
                }
            }
            _ = wait_for_signed_out(&mut auth_state), if workspace_scope == WorkspaceScope::Synced => {
                // Edge transports observe the same auth signal and close at
                // once. Leave a brief reply window for a SignOut RPC before
                // the localhost server itself is aborted.
                runtime.disconnect_edge();
                tracing::info!("headless authentication revoked; stopping synced runtime");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        tracing::info!("shutting down");
        server.abort();
        let _ = server.await;
        if let Some(remote_server) = remote_server {
            remote_server.abort();
            let _ = remote_server.await;
        }
        let clean = runtime.shutdown().await;
        drop(runtime);
        if clean && managed_backup {
            match backup::BackupService::run_managed_offline_if_due(&config.data_dir, now_ms()) {
                Ok(Some(receipt)) => tracing::info!(
                    archive = %receipt.archive.display(),
                    "verified offline backup completed"
                ),
                Ok(None) => {}
                Err(backup::BackupError::UnauthorizedRoot)
                    if !config.data_dir.join("device-id").exists() => {}
                Err(error) => tracing::warn!(%error, "offline backup skipped"),
            }
        }
        Ok(())
    }
}

async fn wait_for_signed_out(state: &mut tokio::sync::watch::Receiver<AuthState>) {
    loop {
        if matches!(&*state.borrow(), AuthState::SignedOut) {
            return;
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}

/// Ctrl-C or SIGTERM. systemd/launchd stop (and the auto-updater's service
/// restart) deliver SIGTERM — without catching it the daemon dies mid-write
/// and every stop takes the crash-recovery path instead of the graceful drain.
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = sigterm.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Serve the typed RPC on the localhost IPC port.
///
/// Both engines call this: the headless daemon, and the headed app's embedded
/// engine. That second case is the point — an embedded engine that keeps the
/// port to itself forces anyone wanting a second viewport (the terminal app) to
/// stop the desktop app, start a daemon, and start it again in the right order.
/// Serving here means any viewport can just attach.
///
/// Localhost only, exactly as before: this widens *which process* can serve the
/// port, not who can reach it.
pub async fn serve_ipc(
    port: u16,
    service: std::sync::Arc<dyn luna_rpc::RpcService>,
    data_root: &Path,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let service = local_owner_ipc_service(service, data_root)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "IPC server listening");
    Ok(tokio::spawn(luna_rpc::serve_ws_listener(listener, service)))
}

/// Block until the WorkOS session is signed in AND org-scoped. On a TTY, print the
/// headless (paste-code) sign-in URL, read the pasted `state.code` from stdin, and
/// run workspace onboarding (create / auto-join / numbered picker). Off a TTY this
/// errors immediately — a daemon under systemd/launchd must load the session that
/// `luna login` persisted, never wait on a prompt nobody can see.
pub async fn terminal_sign_in(auth: &Auth) -> Result<(), EngineError> {
    use std::io::IsTerminal;
    let interactive = std::io::stdin().is_terminal();
    let mut state_rx = auth.watch_state();
    let mut stdin_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut org_reader: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let state = state_rx.borrow().clone();
        match state {
            AuthState::SignedIn { user, org_id } => {
                tracing::info!(email = %user.email, org = org_id.as_deref().unwrap_or("<none>"),
                    "auth: session ready");
                break;
            }
            AuthState::NeedsOrganization { user } => {
                if !interactive {
                    // No reader tasks have been spawned on this path (both spawns
                    // are TTY-gated), so an early return leaks nothing.
                    return Err(EngineError::Other(format!(
                        "signed in as {} but no workspace is selected — run `luna login` on this machine to pick one",
                        user.email
                    )));
                }
                if org_reader.is_none() {
                    // Workspace onboarding on the TTY (old luna's
                    // `backend login` flow): create if none, auto-join a
                    // single membership, numbered picker otherwise.
                    println!("Signed in as {}.", user.email);
                    org_reader = Some(tokio::spawn(run_org_onboarding(auth.clone())));
                }
            }
            AuthState::SignedOut => {
                if !interactive {
                    return Err(EngineError::Other(
                        "not signed in — run `luna login` on this machine first".into(),
                    ));
                }
                if stdin_reader.is_none() {
                    let url = auth.start_headless_sign_in();
                    println!("Sign in to LUNA:\n\n  {url}\n");
                    println!("Then paste the code shown in the browser here and press enter.");
                    let auth = auth.clone();
                    stdin_reader = Some(tokio::spawn(async move {
                        loop {
                            let Some(line) = read_stdin_line().await else {
                                return;
                            };
                            let pasted = line.trim();
                            if pasted.is_empty() {
                                continue;
                            }
                            match auth.complete_sign_in(pasted).await {
                                Ok(()) => return,
                                Err(err) => println!("Sign-in failed: {err}"),
                            }
                        }
                    }));
                }
            }
        }
        if state_rx.changed().await.is_err() {
            break;
        }
    }
    if let Some(reader) = stdin_reader {
        reader.abort();
    }
    if let Some(reader) = org_reader {
        reader.abort();
    }
    Ok(())
}

/// One line from stdin (blocking read off the runtime). `None` = stdin closed.
async fn read_stdin_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None, // EOF / error
            Ok(_) => Some(line),
        }
    })
    .await
    .ok()
    .flatten()
}

/// TTY workspace onboarding for an org-less session (ports old luna's
/// `backend login` flow): no memberships → prompt a name and create; exactly
/// one → auto-join; several → numbered picker. Success flips the auth state to
/// `SignedIn`, which ends [`wait_for_sign_in`]'s wait (and aborts this task).
async fn run_org_onboarding(auth: Auth) {
    let orgs = match auth.list_orgs().await {
        Ok(orgs) => orgs,
        Err(err) => {
            println!(
                "Could not list workspaces ({err}) — create or select one from the LUNA UI to continue."
            );
            return;
        }
    };
    match orgs.len() {
        0 => {
            println!("No workspaces yet — name your new workspace and press enter:");
            loop {
                let Some(line) = read_stdin_line().await else {
                    return;
                };
                let name = line.trim();
                if name.is_empty() {
                    continue;
                }
                match auth.create_org(name).await {
                    Ok(()) => return,
                    Err(err) => println!("Creating workspace failed: {err}"),
                }
            }
        }
        1 => {
            let only = &orgs[0];
            println!("Joining workspace \"{}\"…", only.name);
            if let Err(err) = auth.select_org(&only.organization_id).await {
                println!("Joining workspace failed: {err}");
            }
        }
        _ => {
            println!("\nYour workspaces:");
            for (index, org) in orgs.iter().enumerate() {
                println!("  {}. {}", index + 1, org.name);
            }
            println!("Pick a workspace [1-{}]:", orgs.len());
            loop {
                let Some(line) = read_stdin_line().await else {
                    return;
                };
                let choice = line
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|index| orgs.get(index));
                let Some(org) = choice else {
                    println!("Pick a workspace [1-{}]:", orgs.len());
                    continue;
                };
                match auth.select_org(&org.organization_id).await {
                    Ok(()) => return,
                    Err(err) => println!("Joining workspace failed: {err}"),
                }
            }
        }
    }
}

/// Best-effort human name for this device's registry row.
fn local_device_name(device_id: &str) -> String {
    select_local_device_name(
        [
            std::env::var("LUNA_DEVICE_NAME").ok(),
            native_friendly_device_name(),
            std::env::var("HOSTNAME").ok(),
            gethostname::gethostname().into_string().ok(),
            std::fs::read_to_string("/etc/hostname").ok(),
        ],
        device_id,
        std::env::consts::OS,
    )
}

fn select_local_device_name(
    candidates: impl IntoIterator<Item = Option<String>>,
    device_id: &str,
    platform: &str,
) -> String {
    candidates
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let platform = match platform {
                "macos" => "macOS",
                "windows" => "Windows",
                "linux" => "Linux",
                _ => "Local",
            };
            let short_id: String = device_id.chars().take(8).collect();
            format!("{platform} device {short_id}")
        })
}

#[cfg(target_os = "macos")]
fn native_friendly_device_name() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn native_friendly_device_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    return std::env::var("COMPUTERNAME").ok();

    #[cfg(not(target_os = "windows"))]
    None
}

#[cfg(test)]
mod device_name_tests {
    use super::select_local_device_name;

    fn name(candidates: &[Option<&str>], device_id: &str, platform: &str) -> String {
        select_local_device_name(
            candidates
                .iter()
                .map(|candidate| candidate.map(str::to_string)),
            device_id,
            platform,
        )
    }

    #[test]
    fn explicit_override_wins_and_is_trimmed() {
        assert_eq!(
            name(
                &[Some("  Studio Mac  "), Some("system-host")],
                "17bc0aa2-rest",
                "macos"
            ),
            "Studio Mac"
        );
    }

    #[test]
    fn native_friendly_name_wins_over_hostnames() {
        assert_eq!(
            name(
                &[
                    None,
                    Some("MacBook Pro de Jose"),
                    None,
                    Some("MacBook-Pro.local"),
                ],
                "17bc0aa2-rest",
                "macos"
            ),
            "MacBook Pro de Jose"
        );
    }

    #[test]
    fn windows_computer_name_is_used_when_present() {
        assert_eq!(
            name(
                &[None, Some("DESKTOP-123"), Some("shell-host")],
                "17bc0aa2-rest",
                "windows"
            ),
            "DESKTOP-123"
        );
    }

    #[test]
    fn blank_candidates_are_ignored() {
        assert_eq!(
            name(
                &[Some("  "), None, Some("\n"), Some("linux-box")],
                "17bc0aa2-rest",
                "linux"
            ),
            "linux-box"
        );
    }

    #[test]
    fn final_fallback_is_platform_specific_and_distinct() {
        assert_eq!(
            name(&[None, Some(" ")], "17bc0aa2-rest", "linux"),
            "Linux device 17bc0aa2"
        );
    }
}

/// Trimmed env var or the given default.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Stable per-installation device id, persisted at `{data_dir}/device-id`.
fn load_or_create_device_id(data_dir: &Path) -> Result<String, EngineError> {
    std::fs::create_dir_all(data_dir)?;
    // EngineInfo is resolved before the lifetime InstanceLock is acquired, so
    // identity creation and legacy repair need their own short critical section.
    // The OS releases this lock after a crash; unlike a create_new lockfile it
    // cannot strand an installation permanently.
    let _identity_lock = DeviceIdentityLock::acquire(data_dir)?;
    let path = data_dir.join("device-id");
    let recovering_empty = match std::fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => return Ok(id.trim().to_string()),
        // Older releases used truncate+write. A crash between those operations
        // left a zero-byte file that is safe to replace with a fresh identity.
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };

    let id = new_id();
    let temp_path = data_dir.join(format!(
        ".device-id.tmp-{}-{}",
        std::process::id(),
        new_id()
    ));
    let write_result = (|| -> Result<(), EngineError> {
        let mut temp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        temp.write_all(id.as_bytes())?;
        temp.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    // Fresh installs use create-if-absent. Legacy empty files need an atomic
    // same-directory replacement on Unix; the Windows fallback runs under the
    // identity lock and remains recoverable if interrupted.
    let publish_result = if recovering_empty {
        match std::fs::read_to_string(&path) {
            Ok(id) if !id.trim().is_empty() => {
                let _ = std::fs::remove_file(&temp_path);
                return Ok(id.trim().to_string());
            }
            Ok(_) => replace_empty_device_id(&temp_path, &path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::hard_link(&temp_path, &path)
            }
            Err(err) => Err(err),
        }
    } else {
        std::fs::hard_link(&temp_path, &path)
    };
    let _ = std::fs::remove_file(&temp_path);
    match publish_result {
        Ok(()) => Ok(id),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let winner = std::fs::read_to_string(&path)?;
            if winner.trim().is_empty() {
                Err(EngineError::Other(format!(
                    "invalid device identity {}: file is empty",
                    path.display()
                )))
            } else {
                Ok(winner.trim().to_string())
            }
        }
        Err(err) => Err(err.into()),
    }
}

struct DeviceIdentityLock {
    _file: std::fs::File,
}

impl DeviceIdentityLock {
    fn acquire(data_dir: &Path) -> Result<Self, EngineError> {
        let path = data_dir.join("device-id.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
            let mut retries = 200;
            let file = loop {
                match options.open(&path) {
                    Ok(file) => break file,
                    Err(err)
                        if err.kind() == std::io::ErrorKind::PermissionDenied && retries > 0 =>
                    {
                        retries -= 1;
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(err) => return Err(err.into()),
                }
            };
            return Ok(Self { _file: file });
        }

        #[cfg(not(windows))]
        let file = options.open(&path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EINTR) {
                    return Err(err.into());
                }
            }
        }

        #[cfg(not(windows))]
        Ok(Self { _file: file })
    }
}

fn replace_empty_device_id(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(temp_path, path)
    }
    #[cfg(not(unix))]
    {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        std::fs::hard_link(temp_path, path)
    }
}
