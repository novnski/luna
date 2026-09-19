//! Interactive L08 composer-draft QA fixture.
//!
//! The data root is an explicit command-line argument. Provider homes are
//! synthetic paths below it, the fixture-only mock harness is selected, Auth
//! uses an in-memory credential broker rooted below the fixture, and native
//! provider/Keychain fallbacks are disabled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use sha2::{Digest, Sha256};
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use zeron_harness::{Harness, HarnessError, RunControls, SteerAcceptance};
use zeron_proto::{
    AgentEvent, ChatConfig, ComposerDraft, DoneStatus, DraftBlob, DraftBlobState, DraftContext,
    HarnessId, Model, PermissionMode, ReasoningLevel, RunRequest, SandboxLevel, SteeringMode,
    ToolCall,
};
use zeron_rpc::methods;
use zeron_ui::*;

const TASK_A: &str = "l08-native-a";
const TASK_B: &str = "l08-native-b";
const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

struct L09FixtureHarness {
    hold: Duration,
    steer_outcome: FixtureSteerOutcome,
}

#[derive(Clone, Copy)]
enum FixtureSteerOutcome {
    Incorporated,
    Accepted,
    Rejected,
    Unknown,
}

fn fixture_steer_outcome() -> FixtureSteerOutcome {
    match std::env::var("L09_QA_STEER_OUTCOME")
        .unwrap_or_else(|_| "incorporated".into())
        .as_str()
    {
        "incorporated" => FixtureSteerOutcome::Incorporated,
        "accepted" => FixtureSteerOutcome::Accepted,
        "rejected" => FixtureSteerOutcome::Rejected,
        "unknown" => FixtureSteerOutcome::Unknown,
        other => panic!(
            "unsupported L09_QA_STEER_OUTCOME={other}; use incorporated, accepted, rejected, or unknown"
        ),
    }
}

#[async_trait]
impl Harness for L09FixtureHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "L09 QA Mock"
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    fn deterministic_turn_end(&self) -> bool {
        true
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![Model {
            id: "mock-1".into(),
            label: "L09 QA Mock".into(),
            description: Some("bounded fixture-only synthetic harness".into()),
            reasoning_levels: vec![ReasoningLevel::Medium],
            options: Vec::new(),
        }])
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let hold = self.hold;
        let steer_outcome = self.steer_outcome;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let session_id = format!("l09-qa-{}", uuid::Uuid::new_v4());
            let mut assistant_message_id = format!("l09-qa-assistant-{}", uuid::Uuid::new_v4());
            let _ = tx.send(Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: Vec::new(),
                cwd: request.cwd,
                session_id: session_id.clone(),
                assistant_message_id: assistant_message_id.clone(),
            }));
            let _ = tx.send(Ok(AgentEvent::TextDelta {
                text: "L09 isolated synthetic turn accepted. No provider was contacted.".into(),
            }));
            let _ = tx.send(Ok(AgentEvent::ToolCall {
                id: "l09-qa-tool".into(),
                call: ToolCall::Exec {
                    command: "synthetic-fixture-check".into(),
                },
            }));
            let _ = tx.send(Ok(AgentEvent::ToolResult {
                id: "l09-qa-tool".into(),
                is_error: false,
                output: Some("synthetic tool boundary complete".into()),
                diff: None,
            }));
            let interrupt = controls.interrupt;
            let mut steering = controls.steering;
            let deadline = tokio::time::sleep(hold);
            tokio::pin!(deadline);
            let mut steering_open = true;
            let status = loop {
                tokio::select! {
                    _ = interrupt.cancelled() => break DoneStatus::Interrupted,
                    _ = &mut deadline => break DoneStatus::Completed,
                    steer = steering.recv(), if steering_open => match steer {
                        Some(steer) => {
                            match steer_outcome {
                                FixtureSteerOutcome::Incorporated => {
                                    if let Some(acceptance) = steer.acceptance {
                                        let _ = acceptance.send(SteerAcceptance::Accepted);
                                    }
                                    let next = format!(
                                        "l09-qa-assistant-{}",
                                        uuid::Uuid::new_v4()
                                    );
                                    let _ = tx.send(Ok(AgentEvent::Steered {
                                        assistant_message_id: Some(assistant_message_id.clone()),
                                        next_assistant_message_id: Some(next.clone()),
                                    }));
                                    assistant_message_id = next;
                                    let _ = tx.send(Ok(AgentEvent::TextDelta {
                                        text: format!(
                                            "\nSynthetic provider incorporated: {}",
                                            steer.prompt
                                        ),
                                    }));
                                }
                                FixtureSteerOutcome::Accepted => {
                                    if let Some(acceptance) = steer.acceptance {
                                        let _ = acceptance.send(SteerAcceptance::Accepted);
                                    }
                                    let _ = tx.send(Ok(AgentEvent::TextDelta {
                                        text: "\nSynthetic provider accepted the request without incorporation evidence."
                                            .into(),
                                    }));
                                }
                                FixtureSteerOutcome::Rejected => {
                                    if let Some(acceptance) = steer.acceptance {
                                        let _ = acceptance.send(SteerAcceptance::Rejected(
                                            "synthetic provider rejection".into(),
                                        ));
                                    }
                                }
                                FixtureSteerOutcome::Unknown => {
                                    if let Some(acceptance) = steer.acceptance {
                                        let _ = acceptance.send(SteerAcceptance::Unknown(
                                            "synthetic response was lost".into(),
                                        ));
                                    }
                                }
                            }
                        }
                        None => steering_open = false,
                    }
                }
            };
            let _ = tx.send(Ok(AgentEvent::Done {
                status,
                result: None,
                error: None,
                session_id: Some(session_id),
            }));
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn fixture_hold() -> Duration {
    Duration::from_millis(
        std::env::var("L09_QA_MOCK_HOLD_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(8_000)
            .clamp(1_000, 30_000),
    )
}

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn capture_after_load(
    window: gpui::AnyWindowHandle,
    path: PathBuf,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    cx.background_executor()
        .timer(Duration::from_secs(15))
        .await;
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

fn account_config(root: &Path) -> AgentAccountsConfig {
    let providers = root.join("synthetic-providers");
    AgentAccountsConfig {
        data_dir: providers.clone(),
        claude_config_dir: providers.join("claude"),
        claude_config_file: providers.join("claude.json"),
        codex_home: providers.join("codex"),
        cursor_sdk_auth_file: providers.join("cursor/auth.json"),
    }
}

fn draft(task_id: &str, revision: u64, text: &str) -> ComposerDraft {
    ComposerDraft {
        version: zeron_proto::COMPOSER_DRAFT_VERSION,
        draft_id: task_id.into(),
        task_id: Some(task_id.into()),
        revision,
        text: text.into(),
        attachments: Vec::new(),
        appshots: Vec::new(),
        context: DraftContext::default(),
        execution: serde_json::Value::Null,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    }
}

async fn seed_recovery(core: &EngineCore) -> anyhow::Result<()> {
    let client = zeron_rpc::memory_client(core.rpc_service());
    let png = base64::engine::general_purpose::STANDARD.decode(PNG_B64)?;
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId":"l08-native-recovery",
                "data":PNG_B64,
                "seq":0
            }),
        )
        .await?;
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({
                "uploadId":"l08-native-recovery",
                "fileName":"recovered.png"
            }),
        )
        .await?;
    let path = committed["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("fixture upload returned no path"))?
        .to_string();
    let mut older = draft(TASK_A, 10, "L08 recoverable older message");
    older.attachments.push(DraftBlob {
        attachment_id: "l08-recover-image".into(),
        name: "recovered.png".into(),
        path: Some(path),
        sha256: Some(format!("{:x}", Sha256::digest(&png))),
        size: Some(png.len() as u64),
        state: DraftBlobState::Saved,
        error: None,
    });
    client
        .call(
            methods::BEGIN_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId":"l08-native-recover-command",
                "taskId":TASK_A,
                "snapshot":older
            }),
        )
        .await?;
    client
        .call(
            methods::SETTLE_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId":"l08-native-recover-command",
                "outcome":"recoverable"
            }),
        )
        .await?;
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({
                "draft":draft(TASK_A, 20, "L08 newer draft stays first")
            }),
        )
        .await?;
    Ok(())
}

async fn seed_queue(core: &EngineCore) -> anyhow::Result<()> {
    let client = zeron_rpc::memory_client(core.rpc_service());
    core.doc_host.open(TASK_A)?;
    core.doc_host.pause_all_queues();
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId":"l09-native-attachment",
                "data":PNG_B64,
                "seq":0
            }),
        )
        .await?;
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({
                "uploadId":"l09-native-attachment",
                "fileName":"l09-queue.png"
            }),
        )
        .await?;
    let attachment = committed["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("fixture upload returned no path"))?;
    for (command_id, message_id, text, attachments) in [
        (
            "l09-native-enqueue-edit",
            "l09-native-edit",
            "L09 edit me — attachment must remain owned",
            vec![attachment],
        ),
        (
            "l09-native-enqueue-remove",
            "l09-native-remove",
            "L09 remove me safely",
            Vec::new(),
        ),
        (
            "l09-native-enqueue-send",
            "l09-native-send",
            "L09 send now exactly once",
            Vec::new(),
        ),
    ] {
        let receipt: zeron_proto::CommandReceipt = client
            .call_as(
                methods::EXECUTE_TASK_COMMAND,
                serde_json::json!({
                    "commandId": command_id,
                    "actorId": "l09-native-fixture",
                    "clientId": "l09-native-fixture",
                    "clientSessionId": "isolated",
                    "taskId": TASK_A,
                    "intent": {
                        "kind": "queueMessage",
                        "messageId": message_id,
                        "text": text,
                        "attachments": attachments,
                        "holdForTurnEnd": true,
                        "execution": {
                            "harness": "mock",
                            "providerAccountId": null,
                            "model": "mock-1",
                            "reasoning": null,
                            "modelOptions": {},
                            "sandbox": "workspace-write",
                            "permissionMode": "ask",
                            "effectivePermissionMode": "ask"
                        }
                    }
                }),
            )
            .await?;
        anyhow::ensure!(
            receipt.status == zeron_proto::CommandReceiptStatus::Applied,
            "fixture queue seed was not applied"
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture data root"));
    let mode = std::env::args().nth(2).unwrap_or_else(|| "resume".into());
    let capture_path = std::env::args().nth(3).map(PathBuf::from);
    let engine_data = root.join("engine");
    let ui_data = root.join("ui");
    let temp_data = root.join("tmp");
    std::fs::create_dir_all(&engine_data)?;
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&temp_data)?;
    // This happens before the runtime or any worker thread exists. Keep all
    // library temp files under the explicit fixture boundary.
    unsafe {
        std::env::set_var("TMPDIR", &temp_data);
        std::env::set_var("TMP", &temp_data);
        std::env::set_var("TEMP", &temp_data);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let accounts = account_config(&root);
    accounts.disable_claude_keychain_fallback()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(L09FixtureHarness {
        hold: fixture_hold(),
        steer_outcome: fixture_steer_outcome(),
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "l08-native-org", "l08-native-user"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    let mut auth_config = AuthConfig::new("http://127.0.0.1:1", root.join("auth"));
    auth_config.dev_user_id = "l09-fixture-user".into();
    auth_config.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth_config));
    let existing = core.workspace.read_chats()?;
    for (id, title) in [(TASK_A, "L08 Draft A"), (TASK_B, "L08 Draft B")] {
        if !existing.iter().any(|chat| chat.id == id) {
            core.workspace
                .create_chat(id, None, Some(&core.device_id), None, None)?;
            core.workspace.rename_chat(id, title)?;
        }
    }
    core.workspace.set_chat_config(
        TASK_A,
        &ChatConfig {
            harness: HarnessId::Mock,
            provider_account_id: None,
            model: Some("mock-1".into()),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            permission_mode: PermissionMode::Ask,
            effective_permission_mode: PermissionMode::Ask,
            permission_ceiling_source: None,
            permission_degradation_reason: None,
            permission_audit: Vec::new(),
        },
    )?;
    if mode == "recovery" {
        runtime.block_on(seed_recovery(&core))?;
    } else if mode == "queue" {
        core.workspace.rename_chat(TASK_A, "L09 Durable Queue")?;
        runtime.block_on(seed_queue(&core))?;
    }

    let ipc_port = unused_port();
    let _ipc = runtime.block_on(zeron_engine::serve_ipc(ipc_port, core.rpc_service()))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let device = core.device_id.clone();
    let seed = matches!(mode.as_str(), "seed" | "queue");

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
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
                        "id":device,
                        "name":"Synthetic L08 Mac",
                        "platform":"macos",
                        "lastSeenAt":null
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.chats_synced = true;
                state.spaces_synced = true;
                // Use the production selection transition so project-less
                // task state (`no_project`, selected device, empty-body
                // visibility) is initialized before the first frame.
                state.select_chat(Some(TASK_A.into()), cx);
                // Use the real attachment boundary after selection is known;
                // it starts the transcript and queue watches exactly as the
                // production reconnect path does.
                state.fixture_attach_engine(handle, cx);
                state
            });
            let window = cx
                .open_window(
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
            if seed {
                let composer = window
                    .update(cx, |shell, _, _| shell.fixture_appshots_composer())
                    .unwrap();
                composer.update(cx, |composer, cx| composer.fixture_stage_draft_evidence(cx));
            }
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            if let Some(path) = capture_path {
                cx.spawn(async move |cx| {
                    if let Err(error) = capture_after_load(window.into(), path, cx).await {
                        eprintln!("draft fixture capture failed: {error:#}");
                    }
                })
                .detach();
            }
        });
    runtime.block_on(core.shutdown());
    Ok(())
}
