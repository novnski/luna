//! Hermetic L13 native-history QA fixture.
//!
//! One synthetic Codex-shaped provider backs a completed two-turn task. Native
//! forks return independent ids in a root-scoped synthetic state file. A root-written fake metadata
//! app-server backs archive readback; no installed provider executable, login, Keychain, network
//! service, or real repository is consulted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::{StreamExt, stream::BoxStream};
use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model,
    PermissionMode, ProviderForkBoundary, ProviderForkCapability, ProviderForkRequest,
    ProviderForkResult, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
};
use luna_rpc::{RpcClient, methods};
use luna_ui::*;

const TASK_ID: &str = "13131313-1313-4131-8131-131313131313";
const UNSUPPORTED_TASK_ID: &str = "13131313-1313-4131-8131-131313131314";
const AUTO_CEILING_TASK_ID: &str = "13131313-1313-4131-8131-131313131315";
const LEGACY_ASK_TASK_ID: &str = "13131313-1313-4131-8131-131313131316";
const UNSUPPORTED_USER_MESSAGE_ID: &str = "l13-unsupported-visible-user";
const UNSUPPORTED_ASSISTANT_MESSAGE_ID: &str = "l13-unsupported-visible-assistant";

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct FixtureProviderState {
    sessions: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    metadata: BTreeMap<String, FixtureProviderMetadata>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureProviderMetadata {
    name: String,
    cwd: String,
    archived: bool,
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

    fn refresh(&self, state: &mut FixtureProviderState) -> Result<(), HarnessError> {
        if self.state_path.exists() {
            *state = serde_json::from_slice(&std::fs::read(&self.state_path)?)
                .map_err(|error| HarnessError::Protocol(error.to_string()))?;
        }
        Ok(())
    }

    fn sync_task_metadata(&self, core: &EngineCore, task_id: &str) -> anyhow::Result<()> {
        let Some(chat) = core.workspace.chat(task_id)? else {
            return Ok(());
        };
        let Some(binding) = core
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding)
        else {
            return Ok(());
        };
        if binding.provider_id != HarnessId::Codex || binding.provider_task_id.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap();
        self.refresh(&mut state)?;
        state.metadata.insert(
            binding.provider_task_id,
            FixtureProviderMetadata {
                name: format!("[LUNA] {}", chat.title.as_deref().unwrap_or("New task")),
                cwd: chat.cwd.unwrap_or_else(|| ".".into()),
                archived: chat.archived,
            },
        );
        self.persist(&state)?;
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
        self.refresh(&mut state)?;
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
        let source_metadata = state
            .metadata
            .get(&request.source_session_id)
            .cloned()
            .ok_or_else(|| {
                HarnessError::Protocol(format!(
                    "fixture source metadata is missing for {}",
                    request.source_session_id
                ))
            })?;
        state.metadata.insert(
            provider_session_id.clone(),
            FixtureProviderMetadata {
                cwd: request.cwd,
                archived: false,
                ..source_metadata
            },
        );
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
            self.refresh(&mut state)?;
            let checkpoints = state.sessions.entry(session_id.clone()).or_default();
            checkpoints.push(checkpoint_id.clone());
            let response_number = checkpoints.len();
            state
                .metadata
                .entry(session_id.clone())
                .or_insert_with(|| FixtureProviderMetadata {
                    name: "[LUNA] New task".into(),
                    cwd: request.cwd.clone(),
                    archived: false,
                });
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

fn fixture_registry_with_history(
    root: &Path,
) -> anyhow::Result<(HarnessRegistry, Arc<FixtureHistoryHarness>)> {
    let registry = HarnessRegistry::new();
    let history = Arc::new(FixtureHistoryHarness::open(root)?);
    registry.register(history.clone());
    Ok((registry, history))
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

const FAKE_CODEX_METADATA_SERVER: &str = r#"#!/usr/bin/python3
import json
import os
import pathlib
import sys

root = pathlib.Path(os.environ["CODEX_HOME"]).parent
state_path = root / "provider-state" / "history.json"
trace_path = root / "provider-state" / "metadata-calls.jsonl"

def emit(payload):
    sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def load_state():
    if state_path.exists():
        with state_path.open("r", encoding="utf-8") as handle:
            state = json.load(handle)
    else:
        state = {}
    state.setdefault("sessions", {})
    state.setdefault("metadata", {})
    return state

def save_state(state):
    state_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = state_path.with_suffix(".metadata.tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(temporary, state_path)

def trace(method, params):
    trace_path.parent.mkdir(parents=True, exist_ok=True)
    with trace_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"method": method, "params": params}, sort_keys=True) + "\n")

initialize = json.loads(sys.stdin.readline())
emit({"id": initialize["id"], "result": {"userAgent": "l13-fake-metadata"}})
json.loads(sys.stdin.readline())
request = json.loads(sys.stdin.readline())
request_id = request["id"]
method = request["method"]
params = request.get("params") or {}
trace(method, params)
state = load_state()
metadata = state["metadata"]

if method == "thread/read":
    thread_id = params.get("threadId", "")
    record = metadata.get(thread_id)
    if record is None:
        emit({"id": request_id, "error": {"code": -32602, "message": "thread not found"}})
    else:
        emit({"id": request_id, "result": {"thread": {
            "id": thread_id,
            "name": record["name"],
            "cwd": record["cwd"],
            "status": {"type": "idle"},
            "archived": record["archived"],
            "updatedAt": 1770000000,
            "cliVersion": "0.154.0",
            "projectId": None,
            "ephemeral": False
        }}})
elif method == "thread/list":
    archived = bool(params.get("archived", False))
    search = params.get("searchTerm", "")
    data = [
        {"id": thread_id, "name": record["name"]}
        for thread_id, record in metadata.items()
        if bool(record["archived"]) == archived and (not search or search in record["name"])
    ]
    emit({"id": request_id, "result": {"data": data, "nextCursor": None}})
elif method in ("thread/archive", "thread/unarchive"):
    thread_id = params.get("threadId", "")
    record = metadata.get(thread_id)
    if record is None:
        emit({"id": request_id, "error": {"code": -32602, "message": "thread not found"}})
    else:
        record["archived"] = method == "thread/archive"
        save_state(state)
        emit({"id": request_id, "result": {}})
elif method == "thread/name/set":
    thread_id = params.get("threadId", "")
    record = metadata.get(thread_id)
    if record is None:
        emit({"id": request_id, "error": {"code": -32602, "message": "thread not found"}})
    else:
        record["name"] = params.get("name", "")
        save_state(state)
        emit({"id": request_id, "result": {}})
else:
    emit({"id": request_id, "error": {"code": -32601, "message": "unsupported fixture metadata method"}})
"#;

#[cfg(unix)]
fn fixture_metadata_client(
    root: &Path,
) -> anyhow::Result<luna_harness::codex::native::CodexNativeClient> {
    use std::os::unix::fs::PermissionsExt;

    let executable = root.join("provider-state/fake-codex-metadata.py");
    std::fs::create_dir_all(executable.parent().unwrap())?;
    std::fs::write(&executable, FAKE_CODEX_METADATA_SERVER)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    Ok(
        luna_harness::codex::native::CodexNativeClient::new(root.join("synthetic-codex"))
            .with_executable(executable),
    )
}

#[cfg(not(unix))]
fn fixture_metadata_client(
    _root: &Path,
) -> anyhow::Result<luna_harness::codex::native::CodexNativeClient> {
    anyhow::bail!("the L13 native fixture requires a Unix synthetic metadata executable")
}

async fn execute(
    client: &RpcClient,
    task_id: &str,
    command_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<CommandReceipt> {
    let snapshot: luna_proto::TaskCommandRead = client
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
    history: &FixtureHistoryHarness,
    root: &Path,
    task_id: &str,
    command_prefix: &str,
    title: &str,
    permission_mode: PermissionMode,
    effective_permission_mode: PermissionMode,
    prompts: &[&str],
) -> anyhow::Result<()> {
    if core.workspace.chat(task_id)?.is_none() {
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
    }
    history.sync_task_metadata(core, task_id)?;
    Ok(())
}

fn seed_unsupported_visible_history(core: &EngineCore) -> anyhow::Result<bool> {
    let handle = core.doc_host.open(UNSUPPORTED_TASK_ID)?;
    if !handle.doc().read_entries()?.is_empty() {
        return Ok(false);
    }
    let entries = [
        luna_doc::SessionMessageEntry {
            id: UNSUPPORTED_USER_MESSAGE_ID.into(),
            role: luna_doc::MessageRole::User,
            parts: vec![luna_doc::MessagePart::Text {
                id: "text".into(),
                text: "Prepare a safe synthetic context handoff for a new task draft.".into(),
            }],
            created_at: 1,
            device_id: core.device_id.clone(),
            status: Some(luna_doc::MessageStatus::Complete),
            continuation_of: None,
        },
        luna_doc::SessionMessageEntry {
            id: UNSUPPORTED_ASSISTANT_MESSAGE_ID.into(),
            role: luna_doc::MessageRole::Assistant,
            parts: vec![luna_doc::MessagePart::Text {
                id: "text".into(),
                text: "This completed synthetic conversation is visible context only. Review the opt-in draft before sending it.".into(),
            }],
            created_at: 2,
            device_id: core.device_id.clone(),
            status: Some(luna_doc::MessageStatus::Complete),
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

async fn seed(
    core: &EngineCore,
    history: &FixtureHistoryHarness,
    root: &Path,
) -> anyhow::Result<()> {
    let client = luna_rpc::memory_client(core.rpc_service());
    seed_history_task(
        &client,
        core,
        history,
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
        history,
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
        history,
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
    let (registry, history) = fixture_registry_with_history(&root)?;
    let metadata = fixture_metadata_client(&root)?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_injected_codex_metadata_client(
            EngineProfile::development(&engine_data, "l13-fixture-org", "l13-fixture-user"),
            Arc::new(registry),
            HarnessId::Codex,
            None,
            accounts,
            metadata,
        )
    })?;
    core.set_auth(Auth::new(auth_config(&root)));
    runtime.block_on(seed(&core, &history, &root))?;
    std::fs::write(
        root.join("SAFETY.txt"),
        "L13 synthetic history fixture. In-memory credentials, root-scoped synthetic provider state, and a root-written fake metadata app-server only; no installed/live provider, account, Keychain, network service, or repository access.\n",
    )?;

    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(ipc_port, core.rpc_service()))?;
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

    fn assemble_with_metadata(root: &Path) -> (EngineCore, Arc<FixtureHistoryHarness>) {
        let accounts = account_config(root);
        accounts.disable_claude_keychain_fallback().unwrap();
        let (registry, history) = fixture_registry_with_history(root).unwrap();
        let core = EngineCore::assemble_with_profile_and_injected_codex_metadata_client(
            EngineProfile::development(&root.join("engine"), "l13-test-org", "l13-test-user"),
            Arc::new(registry),
            HarnessId::Codex,
            None,
            accounts,
            fixture_metadata_client(root).unwrap(),
        )
        .unwrap();
        core.set_auth(Auth::new(auth_config(root)));
        (core, history)
    }

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
                    Arc::new(fixture_registry_with_history(root.path()).unwrap().0),
                    HarnessId::Codex,
                    None,
                    accounts,
                )
                .unwrap();
            core.set_auth(Auth::new(auth_config(root.path())));
            core
        };

        let core = assemble();
        let client = luna_rpc::memory_client(core.rpc_service());
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
        assert_eq!(entries[0].role, luna_doc::MessageRole::User);
        assert_eq!(entries[1].role, luna_doc::MessageRole::Assistant);
        assert!(
            entries
                .iter()
                .all(|entry| entry.status == Some(luna_doc::MessageStatus::Complete))
        );
        let handoff = luna_proto::ContextHandoff {
            kind: luna_proto::ContextHandoffKind::RecentVisibleHistory,
            text: entries
                .iter()
                .flat_map(|entry| entry.parts.iter())
                .filter_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
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
        let reopened_client = luna_rpc::memory_client(reopened.rpc_service());
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
        let (core, history) = assemble_with_metadata(root.path());
        seed(&core, &history, root.path()).await.unwrap();

        let handle = core.doc_host.open(TASK_ID).unwrap();
        let entries = handle.doc().read_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.role == luna_doc::MessageRole::User)
                .count(),
            2
        );
        let assistants = entries
            .iter()
            .filter(|entry| entry.role == luna_doc::MessageRole::Assistant)
            .collect::<Vec<_>>();
        assert_eq!(assistants.len(), 2);
        assert!(assistants.iter().all(|entry| {
            entry.status == Some(luna_doc::MessageStatus::Complete)
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
    async fn archive_round_trip_preserves_independent_child_and_fake_metadata_restart() {
        let root = tempfile::tempdir().unwrap();
        let (core, history) = assemble_with_metadata(root.path());
        seed(&core, &history, root.path()).await.unwrap();
        let client = luna_rpc::memory_client(core.rpc_service());
        let source_chat = core.workspace.chat(TASK_ID).unwrap().unwrap();
        let source_binding = core
            .task_identity
            .view(TASK_ID)
            .unwrap()
            .current_binding
            .unwrap();
        let source_snapshot: luna_proto::TaskCommandRead = client
            .call_as(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                serde_json::json!({ "taskId": TASK_ID }),
            )
            .await
            .unwrap();
        let child_id = uuid::Uuid::new_v4().to_string();
        let review = serde_json::json!({
            "taskRevision": source_snapshot.snapshot.task_revision,
            "bindingGeneration": source_snapshot.snapshot.binding_generation.unwrap(),
            "providerSessionId": source_binding.provider_task_id.clone(),
            "providerAccountId": source_chat.config.as_ref().and_then(|config| config.provider_account_id.clone()),
            "cwd": source_chat.cwd.clone().unwrap(),
            "messageId": null,
            "messageText": null,
        });
        let forked = execute(
            &client,
            TASK_ID,
            "l13-metadata-child-fork",
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": child_id.clone(),
                "boundary": "tip",
                "title": source_chat.title,
                "config": source_chat.config,
                "sourceLink": format!("luna://open/chat/{TASK_ID}?workspace=fixture"),
                "review": review,
            }),
        )
        .await
        .unwrap();
        assert_eq!(forked.status, CommandReceiptStatus::Applied, "{forked:?}");
        let child_binding = core
            .task_identity
            .view(&child_id)
            .unwrap()
            .current_binding
            .unwrap();
        assert_ne!(
            source_binding.provider_task_id,
            child_binding.provider_task_id
        );

        let archived = execute(
            &client,
            TASK_ID,
            "l13-metadata-archive",
            serde_json::json!({ "kind": "setArchived", "archived": true }),
        )
        .await
        .unwrap();
        assert_eq!(archived.status, CommandReceiptStatus::Applied);
        assert!(core.workspace.chat(TASK_ID).unwrap().unwrap().archived);
        assert!(!core.workspace.chat(&child_id).unwrap().unwrap().archived);

        let child_run = execute(
            &client,
            &child_id,
            "l13-metadata-child-run",
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "request": run_request(
                        root.path(),
                        "Continue the independent child while its source is archived.",
                        PermissionMode::Full,
                    ),
                    "messageId": "l13-metadata-child-user",
                }
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            child_run.status,
            CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
        ));
        wait_applied(&client, "l13-metadata-child-run")
            .await
            .unwrap();
        let child_entries = core
            .doc_host
            .open(&child_id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap();
        assert!(child_entries.iter().any(|entry| {
            entry.parts.iter().any(|part| {
                matches!(part, luna_doc::MessagePart::Text { text, .. }
                    if text == "Continue the independent child while its source is archived.")
            })
        }));
        assert!(
            !core
                .doc_host
                .open(TASK_ID)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap()
                .iter()
                .any(|entry| entry.id == "l13-metadata-child-user")
        );

        let unarchived = execute(
            &client,
            TASK_ID,
            "l13-metadata-unarchive",
            serde_json::json!({ "kind": "setArchived", "archived": false }),
        )
        .await
        .unwrap();
        assert_eq!(unarchived.status, CommandReceiptStatus::Applied);
        assert!(!core.workspace.chat(TASK_ID).unwrap().unwrap().archived);
        let before_restart: FixtureProviderState = serde_json::from_slice(
            &std::fs::read(root.path().join("provider-state/history.json")).unwrap(),
        )
        .unwrap();
        assert!(!before_restart.metadata[&source_binding.provider_task_id].archived);
        assert!(!before_restart.metadata[&child_binding.provider_task_id].archived);

        drop(client);
        core.shutdown().await;
        drop(core);
        drop(history);

        let (reopened, reopened_history) = assemble_with_metadata(root.path());
        seed(&reopened, &reopened_history, root.path())
            .await
            .unwrap();
        assert!(!reopened.workspace.chat(TASK_ID).unwrap().unwrap().archived);
        assert!(
            !reopened
                .workspace
                .chat(&child_id)
                .unwrap()
                .unwrap()
                .archived
        );
        assert_eq!(
            reopened
                .task_identity
                .view(&child_id)
                .unwrap()
                .current_binding
                .unwrap()
                .provider_task_id,
            child_binding.provider_task_id
        );
        assert!(
            reopened
                .doc_host
                .open(&child_id)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap()
                .iter()
                .any(|entry| entry.id == "l13-metadata-child-user")
        );

        let injected = fixture_metadata_client(root.path()).unwrap();
        assert!(
            injected
                .observe_thread("missing-fixture-thread")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            injected
                .set_archived("missing-fixture-thread", true)
                .await
                .is_err()
        );
        let after_missing: FixtureProviderState = serde_json::from_slice(
            &std::fs::read(root.path().join("provider-state/history.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(after_missing.sessions, before_restart.sessions);
        assert_eq!(
            after_missing.metadata.keys().collect::<Vec<_>>(),
            before_restart.metadata.keys().collect::<Vec<_>>(),
            "missing-id errors must not invent provider state or fall back elsewhere"
        );
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn disabled_metadata_assembly_never_invokes_a_provider_fallback() {
        let root = tempfile::tempdir().unwrap();
        let accounts = account_config(root.path());
        accounts.disable_claude_keychain_fallback().unwrap();
        let (registry, history) = fixture_registry_with_history(root.path()).unwrap();
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(
                    &root.path().join("engine"),
                    "l13-test-org",
                    "l13-test-user",
                ),
                Arc::new(registry),
                HarnessId::Codex,
                None,
                accounts,
            )
            .unwrap();
        core.set_auth(Auth::new(auth_config(root.path())));
        seed(&core, &history, root.path()).await.unwrap();
        let client = luna_rpc::memory_client(core.rpc_service());
        let receipt = execute(
            &client,
            TASK_ID,
            "l13-disabled-metadata-archive",
            serde_json::json!({ "kind": "setArchived", "archived": true }),
        )
        .await
        .unwrap();
        assert_eq!(receipt.status, CommandReceiptStatus::Pending);
        assert_eq!(
            receipt.reason.unwrap().code,
            luna_proto::CommandReasonCode::ProviderAcceptanceUnknown
        );
        assert!(!core.workspace.chat(TASK_ID).unwrap().unwrap().archived);
        assert!(
            !root
                .path()
                .join("provider-state/metadata-calls.jsonl")
                .exists()
        );
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
            state.metadata.insert(
                "source".into(),
                FixtureProviderMetadata {
                    name: "[LUNA] Source".into(),
                    cwd: root.path().to_string_lossy().into_owned(),
                    archived: false,
                },
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
