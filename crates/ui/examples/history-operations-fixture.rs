//! Hermetic L13 native-history QA fixture.
//!
//! One synthetic Codex-shaped provider backs a completed two-turn task. Native
//! forks return independent ids in a root-scoped synthetic state file; no provider executable, login,
//! Keychain, network service, or real repository is consulted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::{StreamExt, stream::BoxStream};
use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, ChatConfig, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model,
    PermissionMode, ProviderForkBoundary, ProviderForkCapability, ProviderForkRequest,
    ProviderForkResult, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
};
use zeron_rpc::{RpcClient, methods};
use zeron_ui::*;

const TASK_ID: &str = "13131313-1313-4131-8131-131313131313";
const UNSUPPORTED_TASK_ID: &str = "13131313-1313-4131-8131-131313131314";
const AUTO_CEILING_TASK_ID: &str = "13131313-1313-4131-8131-131313131315";
const LEGACY_ASK_TASK_ID: &str = "13131313-1313-4131-8131-131313131316";
const UNSUPPORTED_USER_MESSAGE_ID: &str = "l13-unsupported-visible-user";
const UNSUPPORTED_ASSISTANT_MESSAGE_ID: &str = "l13-unsupported-visible-assistant";

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct FixtureProviderState {
    sessions: BTreeMap<String, Vec<String>>,
}

struct FixtureHistoryHarness {
    state_path: PathBuf,
    state: Mutex<FixtureProviderState>,
}

impl FixtureHistoryHarness {
    fn open(root: &Path) -> anyhow::Result<Self> {
        let state_path = root.join("provider-state/history.json");
        let state = if state_path.exists() {
            serde_json::from_slice(&std::fs::read(&state_path)?)?
        } else {
            FixtureProviderState::default()
        };
        Ok(Self {
            state_path,
            state: Mutex::new(state),
        })
    }

    fn persist(&self, state: &FixtureProviderState) -> Result<(), HarnessError> {
        let parent = self
            .state_path
            .parent()
            .ok_or_else(|| HarnessError::Protocol("fixture state has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!("history-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(
            &temporary,
            serde_json::to_vec_pretty(state)
                .map_err(|error| HarnessError::Protocol(error.to_string()))?,
        )?;
        std::fs::rename(temporary, &self.state_path)?;
        Ok(())
    }
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
        let mut state = self.state.lock().unwrap();
        let source = state
            .sessions
            .get(&request.source_session_id)
            .ok_or_else(|| {
                HarnessError::Protocol(format!(
                    "unknown fixture source session {}",
                    request.source_session_id
                ))
            })?
            .clone();
        let prefix_len = match &request.boundary {
            ProviderForkBoundary::Tip => source.len(),
            ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => source
                .iter()
                .position(|candidate| candidate == checkpoint_id)
                .ok_or_else(|| {
                    HarnessError::Protocol(format!("unknown fixture checkpoint {checkpoint_id}"))
                })?,
            ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => source
                .iter()
                .position(|candidate| candidate == checkpoint_id)
                .map(|index| index + 1)
                .ok_or_else(|| {
                    HarnessError::Protocol(format!("unknown fixture checkpoint {checkpoint_id}"))
                })?,
        };
        let provider_session_id = format!("l13-fixture-{}", uuid::Uuid::new_v4());
        let mut checkpoints = Vec::with_capacity(prefix_len);
        let mut checkpoint_map = BTreeMap::new();
        for source_checkpoint in &source[..prefix_len] {
            let target_checkpoint = format!("l13-checkpoint-{}", uuid::Uuid::new_v4());
            checkpoint_map.insert(source_checkpoint.clone(), target_checkpoint.clone());
            checkpoints.push(target_checkpoint);
        }
        state
            .sessions
            .insert(provider_session_id.clone(), checkpoints);
        self.persist(&state)?;
        Ok(ProviderForkResult {
            provider_session_id,
            checkpoint_map,
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
        let session_id = request
            .resume
            .unwrap_or_else(|| format!("l13-fixture-{}", uuid::Uuid::new_v4()));
        let assistant_message_id = format!("l13-assistant-{}", uuid::Uuid::new_v4());
        let checkpoint_id = format!("l13-checkpoint-{}", uuid::Uuid::new_v4());
        let response_number = {
            let mut state = self.state.lock().unwrap();
            let checkpoints = state.sessions.entry(session_id.clone()).or_default();
            checkpoints.push(checkpoint_id.clone());
            let response_number = checkpoints.len();
            self.persist(&state)?;
            response_number
        };
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Codex,
                model: "synthetic-l13".into(),
                tools: Vec::new(),
                cwd: request.cwd,
                session_id: session_id.clone(),
                assistant_message_id,
            }),
            Ok(AgentEvent::TextDelta {
                text: format!(
                    "Synthetic completed response {response_number}. Hover this turn to fork after it; hover the user message to edit or fork before it."
                ),
            }),
            Ok(AgentEvent::ProviderCheckpoint {
                checkpoint_id,
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

fn chat_config(
    harness: HarnessId,
    permission_mode: PermissionMode,
    effective_permission_mode: PermissionMode,
) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model: Some("synthetic-l13".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode,
        effective_permission_mode,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn fixture_registry(root: &Path) -> anyhow::Result<HarnessRegistry> {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FixtureHistoryHarness::open(root)?));
    Ok(registry)
}

fn run_request(root: &Path, prompt: &str, permission_mode: PermissionMode) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Codex),
        provider_account_id: None,
        model: Some("synthetic-l13".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        cwd: root.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: permission_mode == PermissionMode::Full,
        permission_mode,
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
    task_id: &str,
    command_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<CommandReceipt> {
    let snapshot: zeron_proto::TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await?;
    let mut envelope = serde_json::json!({
        "commandId": command_id,
        "actorId": "l13-native-fixture",
        "clientId": "l13-native-fixture",
        "clientSessionId": "l13-native-fixture-bootstrap",
        "taskId": task_id,
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

async fn seed_history_task(
    client: &RpcClient,
    core: &EngineCore,
    root: &Path,
    task_id: &str,
    command_prefix: &str,
    title: &str,
    permission_mode: PermissionMode,
    effective_permission_mode: PermissionMode,
    prompts: &[&str],
) -> anyhow::Result<()> {
    if core.workspace.chat(task_id)?.is_some() {
        return Ok(());
    }
    let created = execute(
        client,
        task_id,
        &format!("{command_prefix}-create"),
        serde_json::json!({
            "kind": "createChat",
            "deviceId": core.device_id,
            "cwd": root,
            "config": chat_config(
                HarnessId::Codex,
                permission_mode,
                effective_permission_mode,
            ),
        }),
    )
    .await?;
    anyhow::ensure!(created.status == CommandReceiptStatus::Applied);
    core.workspace.rename_chat(task_id, title)?;
    for (index, prompt) in prompts.iter().enumerate() {
        let command_id = format!("{command_prefix}-turn-{index}");
        execute(
            client,
            task_id,
            &command_id,
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": run_request(root, prompt, effective_permission_mode),
                    "messageId": format!("{command_prefix}-user-{index}"),
                }
            }),
        )
        .await?;
        wait_applied(client, &command_id).await?;
    }
    Ok(())
}

fn seed_unsupported_visible_history(core: &EngineCore) -> anyhow::Result<bool> {
    let handle = core.doc_host.open(UNSUPPORTED_TASK_ID)?;
    if !handle.doc().read_entries()?.is_empty() {
        return Ok(false);
    }
    let entries = [
        zeron_doc::SessionMessageEntry {
            id: UNSUPPORTED_USER_MESSAGE_ID.into(),
            role: zeron_doc::MessageRole::User,
            parts: vec![zeron_doc::MessagePart::Text {
                id: "text".into(),
                text: "Prepare a safe synthetic context handoff for a new task draft.".into(),
            }],
            created_at: 1,
            device_id: core.device_id.clone(),
            status: Some(zeron_doc::MessageStatus::Complete),
            continuation_of: None,
        },
        zeron_doc::SessionMessageEntry {
            id: UNSUPPORTED_ASSISTANT_MESSAGE_ID.into(),
            role: zeron_doc::MessageRole::Assistant,
            parts: vec![zeron_doc::MessagePart::Text {
                id: "text".into(),
                text: "This completed synthetic conversation is visible context only. Review the opt-in draft before sending it.".into(),
            }],
            created_at: 2,
            device_id: core.device_id.clone(),
            status: Some(zeron_doc::MessageStatus::Complete),
            continuation_of: None,
        },
    ];
    handle
        .doc()
        .replace_conversation_entries(&entries, &std::collections::HashMap::new())?;
    core.doc_host.flush_all();
    Ok(true)
}

async fn seed_unsupported_task(
    client: &RpcClient,
    core: &EngineCore,
    root: &Path,
) -> anyhow::Result<()> {
    if core.workspace.chat(UNSUPPORTED_TASK_ID)?.is_none() {
        let unsupported = execute(
            client,
            UNSUPPORTED_TASK_ID,
            "l13-create-unsupported",
            serde_json::json!({
                "kind": "createChat",
                "deviceId": core.device_id,
                "cwd": root,
                "config": chat_config(
                    HarnessId::ClaudeCode,
                    PermissionMode::Ask,
                    PermissionMode::Ask,
                ),
            }),
        )
        .await?;
        anyhow::ensure!(unsupported.status == CommandReceiptStatus::Applied);
        core.workspace
            .rename_chat(UNSUPPORTED_TASK_ID, "L13 unsupported-provider fallback QA")?;
    }
    // Directly seed only a truly empty fixture document. This backfills the
    // previously audited root without invoking or registering Claude, while
    // preserving any existing task history byte-for-byte.
    seed_unsupported_visible_history(core)?;
    Ok(())
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let client = zeron_rpc::memory_client(core.rpc_service());
    seed_history_task(
        &client,
        core,
        root,
        TASK_ID,
        "l13-full",
        "L13 Full permission fork and edit QA",
        PermissionMode::Full,
        PermissionMode::Full,
        &[
            "Draft a safe provider-backed fork plan.",
            "Now preserve the original task while exploring an alternative.",
        ],
    )
    .await?;
    seed_history_task(
        &client,
        core,
        root,
        AUTO_CEILING_TASK_ID,
        "l13-auto-ceiling",
        "L13 Auto-only permission ceiling QA",
        PermissionMode::Full,
        PermissionMode::Auto,
        &["Exercise a fork whose current permission ceiling is Auto."],
    )
    .await?;
    seed_history_task(
        &client,
        core,
        root,
        LEGACY_ASK_TASK_ID,
        "l13-legacy-ask",
        "L13 legacy Ask read-only permission QA",
        PermissionMode::Ask,
        PermissionMode::Ask,
        &["Exercise a legacy Ask fork without making Ask selectable."],
    )
    .await?;
    seed_unsupported_task(&client, core, root).await?;
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
    let registry = fixture_registry(&root)?;
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
        "L13 synthetic history fixture. In-memory credentials and root-scoped synthetic provider state only; no live provider, account, Keychain, network service, or repository access.\n",
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
    async fn unsupported_task_backfills_visible_handoff_without_provider_invocation() {
        let root = tempfile::tempdir().unwrap();
        let assemble = || {
            let accounts = account_config(root.path());
            accounts.disable_claude_keychain_fallback().unwrap();
            let core =
                EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                    EngineProfile::development(
                        &root.path().join("engine"),
                        "l13-test-org",
                        "l13-test-user",
                    ),
                    Arc::new(fixture_registry(root.path()).unwrap()),
                    HarnessId::Codex,
                    None,
                    accounts,
                )
                .unwrap();
            core.set_auth(Auth::new(auth_config(root.path())));
            core
        };

        let core = assemble();
        let client = zeron_rpc::memory_client(core.rpc_service());
        let created = execute(
            &client,
            UNSUPPORTED_TASK_ID,
            "l13-create-unsupported",
            serde_json::json!({
                "kind": "createChat",
                "deviceId": core.device_id,
                "cwd": root.path(),
                "config": chat_config(
                    HarnessId::ClaudeCode,
                    PermissionMode::Ask,
                    PermissionMode::Ask,
                ),
            }),
        )
        .await
        .unwrap();
        assert_eq!(created.status, CommandReceiptStatus::Applied);
        core.workspace
            .rename_chat(UNSUPPORTED_TASK_ID, "L13 unsupported-provider fallback QA")
            .unwrap();
        assert!(
            core.doc_host
                .open(UNSUPPORTED_TASK_ID)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap()
                .is_empty(),
            "the audited pre-correction fixture root had an empty unsupported task"
        );

        seed_unsupported_task(&client, &core, root.path())
            .await
            .unwrap();
        let entries = core
            .doc_host
            .open(UNSUPPORTED_TASK_ID)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, zeron_doc::MessageRole::User);
        assert_eq!(entries[1].role, zeron_doc::MessageRole::Assistant);
        assert!(
            entries
                .iter()
                .all(|entry| entry.status == Some(zeron_doc::MessageStatus::Complete))
        );
        let handoff = zeron_proto::ContextHandoff {
            kind: zeron_proto::ContextHandoffKind::RecentVisibleHistory,
            text: entries
                .iter()
                .flat_map(|entry| entry.parts.iter())
                .filter_map(|part| match part {
                    zeron_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
            message_ids: entries.iter().map(|entry| entry.id.clone()).collect(),
        };
        assert!(!handoff.text.is_empty());
        assert_eq!(
            handoff.message_ids,
            [
                UNSUPPORTED_USER_MESSAGE_ID.to_string(),
                UNSUPPORTED_ASSISTANT_MESSAGE_ID.to_string(),
            ]
        );
        let unsupported = core.workspace.chat(UNSUPPORTED_TASK_ID).unwrap().unwrap();
        assert_eq!(
            unsupported.config.unwrap().harness,
            HarnessId::ClaudeCode,
            "the fallback must remain explicitly unsupported for native history"
        );
        assert!(core.registry.resolve(HarnessId::ClaudeCode).is_err());
        assert!(
            !root.path().join("provider-state/history.json").exists(),
            "seeding visible fallback context must not invoke the registered synthetic provider"
        );

        seed_unsupported_task(&client, &core, root.path())
            .await
            .unwrap();
        assert_eq!(
            core.doc_host
                .open(UNSUPPORTED_TASK_ID)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap(),
            entries,
            "same-process reseeding must preserve existing history"
        );
        drop(client);
        core.shutdown().await;
        drop(core);

        let reopened = assemble();
        let reopened_client = zeron_rpc::memory_client(reopened.rpc_service());
        seed_unsupported_task(&reopened_client, &reopened, root.path())
            .await
            .unwrap();
        assert_eq!(
            reopened
                .doc_host
                .open(UNSUPPORTED_TASK_ID)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap(),
            entries,
            "restart reseeding must preserve the already-backfilled history"
        );
        assert!(reopened.registry.resolve(HarnessId::ClaudeCode).is_err());
        assert!(!root.path().join("provider-state/history.json").exists());
        reopened.shutdown().await;
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
                Arc::new(fixture_registry(root.path()).unwrap()),
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
        let full = core
            .workspace
            .chat(TASK_ID)
            .unwrap()
            .unwrap()
            .config
            .unwrap();
        assert_eq!(full.permission_mode, PermissionMode::Full);
        assert_eq!(full.effective_permission_mode, PermissionMode::Full);
        let auto_ceiling = core
            .workspace
            .chat(AUTO_CEILING_TASK_ID)
            .unwrap()
            .unwrap()
            .config
            .unwrap();
        assert_eq!(auto_ceiling.permission_mode, PermissionMode::Full);
        assert_eq!(auto_ceiling.effective_permission_mode, PermissionMode::Auto);
        let legacy = core
            .workspace
            .chat(LEGACY_ASK_TASK_ID)
            .unwrap()
            .unwrap()
            .config
            .unwrap();
        assert_eq!(legacy.permission_mode, PermissionMode::Ask);
        assert_eq!(legacy.effective_permission_mode, PermissionMode::Ask);
        core.shutdown().await;
    }

    #[tokio::test]
    async fn fixture_provider_persists_collision_safe_boundary_forks() {
        let root = tempfile::tempdir().unwrap();
        let harness = FixtureHistoryHarness::open(root.path()).unwrap();
        {
            let mut state = harness.state.lock().unwrap();
            state.sessions.insert(
                "source".into(),
                vec!["checkpoint-a".into(), "checkpoint-b".into()],
            );
            harness.persist(&state).unwrap();
        }
        let request = |source_session_id: String, boundary| ProviderForkRequest {
            source_session_id,
            boundary,
            cwd: root.path().to_string_lossy().into_owned(),
            model: None,
            provider_home: None,
            force_file_auth: true,
        };
        let first = harness
            .fork_session(request(
                "source".into(),
                ProviderForkBoundary::ThroughCheckpoint {
                    checkpoint_id: "checkpoint-a".into(),
                },
            ))
            .await
            .unwrap();
        assert_eq!(first.checkpoint_map.len(), 1);
        assert_ne!(
            first.checkpoint_map["checkpoint-a"],
            "checkpoint-a".to_string()
        );

        let reopened = FixtureHistoryHarness::open(root.path()).unwrap();
        let second = reopened
            .fork_session(request(
                first.provider_session_id.clone(),
                ProviderForkBoundary::Tip,
            ))
            .await
            .unwrap();
        assert_ne!(first.provider_session_id, second.provider_session_id);
        assert_eq!(second.checkpoint_map.len(), 1);
        assert!(
            reopened
                .fork_session(request(
                    "source".into(),
                    ProviderForkBoundary::ThroughCheckpoint {
                        checkpoint_id: "missing".into(),
                    },
                ))
                .await
                .is_err()
        );
    }
}
