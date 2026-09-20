//! Hermetic L13 native-history QA fixture.
//!
//! One synthetic Codex-shaped provider backs a completed two-turn task. Native
//! forks return independent in-memory ids; no provider executable, login,
//! Keychain, network service, or real repository is consulted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{StreamExt, stream::BoxStream};
use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, ChatConfig, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model,
    PermissionMode, ProviderForkCapability, ProviderForkRequest, ProviderForkResult,
    ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
};
use zeron_rpc::{RpcClient, methods};
use zeron_ui::*;

const TASK_ID: &str = "13131313-1313-4131-8131-131313131313";

struct FixtureHistoryHarness {
    run_sequence: AtomicUsize,
    fork_sequence: AtomicUsize,
}

#[async_trait::async_trait]
impl Harness for FixtureHistoryHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }

    fn display_name(&self) -> &str {
        "Synthetic L13 history provider"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    fn fork_capability(&self) -> ProviderForkCapability {
        ProviderForkCapability::Checkpoint
    }

    async fn fork_session(
        &self,
        request: ProviderForkRequest,
    ) -> Result<ProviderForkResult, HarnessError> {
        let sequence = self.fork_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ProviderForkResult {
            provider_session_id: format!(
                "l13-fixture-fork-{sequence}-from-{}",
                request.source_session_id
            ),
        })
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let sequence = self.run_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let session_id = request
            .resume
            .unwrap_or_else(|| "l13-fixture-source-session".into());
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Codex,
                model: "synthetic-l13".into(),
                tools: Vec::new(),
                cwd: request.cwd,
                session_id: session_id.clone(),
                assistant_message_id: format!("l13-assistant-{sequence}"),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!(
                    "Synthetic completed response {sequence}. Hover this turn to fork after it; hover the user message to edit or fork before it."
                ),
            }),
            Ok(AgentEvent::ProviderCheckpoint {
                checkpoint_id: format!("l13-provider-turn-{sequence}"),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session_id),
            }),
        ])
        .boxed())
    }
}

fn account_config(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

fn auth_config(root: &Path) -> AuthConfig {
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("synthetic-auth"));
    config.dev_user_id = "l13-fixture-user".into();
    config.use_in_memory_credentials_for_tests();
    config
}

fn chat_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Codex,
        provider_account_id: None,
        model: Some("synthetic-l13".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn fixture_registry() -> HarnessRegistry {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FixtureHistoryHarness {
        run_sequence: AtomicUsize::new(0),
        fork_sequence: AtomicUsize::new(0),
    }));
    registry
}

fn run_request(root: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Codex),
        provider_account_id: None,
        model: Some("synthetic-l13".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        cwd: root.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

async fn execute(
    client: &RpcClient,
    command_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<CommandReceipt> {
    let snapshot: zeron_proto::TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": TASK_ID }),
        )
        .await?;
    let mut envelope = serde_json::json!({
        "commandId": command_id,
        "actorId": "l13-native-fixture",
        "clientId": "l13-native-fixture",
        "clientSessionId": "l13-native-fixture-bootstrap",
        "taskId": TASK_ID,
        "expectedTaskRevision": snapshot.snapshot.task_revision,
        "intent": intent,
    });
    if let Some(generation) = snapshot.snapshot.binding_generation {
        envelope["expectedBindingGeneration"] = generation.into();
    }
    Ok(client
        .call_as(methods::EXECUTE_TASK_COMMAND, envelope)
        .await?)
}

async fn wait_applied(client: &RpcClient, command_id: &str) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let receipt: Option<CommandReceipt> = client
                .call_as(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    serde_json::json!({ "commandId": command_id }),
                )
                .await?;
            if let Some(receipt) = receipt {
                match receipt.status {
                    CommandReceiptStatus::Applied => return Ok(()),
                    CommandReceiptStatus::Pending => {}
                    status => {
                        anyhow::bail!("fixture command ended {status:?}: {:?}", receipt.reason)
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    if core.workspace.chat(TASK_ID)?.is_some() {
        return Ok(());
    }
    let client = zeron_rpc::memory_client(core.rpc_service());
    let created = execute(
        &client,
        "l13-create",
        serde_json::json!({
            "kind": "createChat",
            "deviceId": core.device_id,
            "cwd": root,
            "config": chat_config(),
        }),
    )
    .await?;
    anyhow::ensure!(created.status == CommandReceiptStatus::Applied);
    core.workspace
        .rename_chat(TASK_ID, "L13 native fork and edit QA")?;

    for (index, prompt) in [
        "Draft a safe provider-backed fork plan.",
        "Now preserve the original task while exploring an alternative.",
    ]
    .into_iter()
    .enumerate()
    {
        let command_id = format!("l13-seed-turn-{index}");
        execute(
            &client,
            &command_id,
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": run_request(root, prompt),
                    "messageId": format!("l13-user-{index}"),
                }
            }),
        )
        .await?;
        wait_applied(&client, &command_id).await?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    anyhow::ensure!(root.is_absolute(), "fixture root must be absolute");
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let engine_data = root.join("engine");
    let ui_data = root.join("ui");
    let temp_data = root.join("tmp");
    std::fs::create_dir_all(&engine_data)?;
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&temp_data)?;
    unsafe {
        std::env::set_var("TMPDIR", &temp_data);
        std::env::set_var("TMP", &temp_data);
        std::env::set_var("TEMP", &temp_data);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let accounts = account_config(&root);
    accounts.disable_claude_keychain_fallback()?;
    let registry = fixture_registry();
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(&engine_data, "l13-fixture-org", "l13-fixture-user"),
            Arc::new(registry),
            HarnessId::Codex,
            None,
            accounts,
        )
    })?;
    core.set_auth(Auth::new(auth_config(&root)));
    runtime.block_on(seed(&core, &root))?;
    std::fs::write(
        root.join("SAFETY.txt"),
        "L13 synthetic history fixture. In-memory credentials and provider only; no live provider, account, Keychain, network store, or repository access.\n",
    )?;

    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(zeron_engine::serve_ipc(ipc_port, core.rpc_service()))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Codex,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let device = core.device_id.clone();

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::load(&ui_data);
            settings::init(settings.clone(), ui_data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family.clone(),
                settings.ui_font_size,
                settings.terminal_font_family.clone(),
                settings.terminal_font_size,
                settings.code_font_family.clone(),
                settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(ui_data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            history::init(
                settings.git_history_columns,
                settings.git_history_column_widths,
                settings.git_history_column_order,
                settings.git_history_author_display,
                cx,
            );
            composer::init(cx, settings.composer_send_behavior);
            terminal::panel::init(cx);
            app_menus::init(cx);
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device,
                        "name": "Synthetic L13 Mac",
                        "platform": "macos",
                        "lastSeenAt": null,
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.select_chat(Some(TASK_ID.into()), cx);
                state.fixture_attach_engine(handle, cx);
                state
            });
            cx.open_window(
                WindowOptions {
                    window_background: theme::Theme::of(cx).window_background_appearance(),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(80.), px(80.)),
                        size(px(1100.), px(850.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
            )
            .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
        });
    runtime.block_on(core.shutdown());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_auth_is_in_memory_only() {
        let root = tempfile::tempdir().unwrap();
        let config = auth_config(root.path());
        assert_eq!(config.dev_user_id, "l13-fixture-user");
        assert!(
            config
                .test_credential_broker()
                .independent_in_memory_handle_for_tests()
                .is_some()
        );
    }

    #[tokio::test]
    async fn fixture_seed_uses_only_synthetic_completed_history() {
        let root = tempfile::tempdir().unwrap();
        let accounts = account_config(root.path());
        accounts.disable_claude_keychain_fallback().unwrap();
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(
                    &root.path().join("engine"),
                    "l13-test-org",
                    "l13-test-user",
                ),
                Arc::new(fixture_registry()),
                HarnessId::Codex,
                None,
                accounts,
            )
            .unwrap();
        core.set_auth(Auth::new(auth_config(root.path())));
        seed(&core, root.path()).await.unwrap();

        let handle = core.doc_host.open(TASK_ID).unwrap();
        let entries = handle.doc().read_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.role == zeron_doc::MessageRole::User)
                .count(),
            2
        );
        let assistants = entries
            .iter()
            .filter(|entry| entry.role == zeron_doc::MessageRole::Assistant)
            .collect::<Vec<_>>();
        assert_eq!(assistants.len(), 2);
        assert!(assistants.iter().all(|entry| {
            entry.status == Some(zeron_doc::MessageStatus::Complete)
                && handle.doc().provider_checkpoint(&entry.id).is_some()
        }));
        assert_eq!(
            core.registry
                .resolve(HarnessId::Codex)
                .unwrap()
                .fork_capability(),
            ProviderForkCapability::Checkpoint
        );
        core.shutdown().await;
    }
}
