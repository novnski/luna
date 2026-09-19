//! Interactive L08 composer-draft QA fixture.
//!
//! The data root is an explicit command-line argument. Provider homes are
//! synthetic paths below it, the fixture-only mock harness is selected, Auth
//! uses an in-memory credential broker rooted below the fixture, and native
//! provider/Keychain fallbacks are disabled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{fs::OpenOptions, io::Write as _};

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
    safety_deadline: Duration,
    steer_outcome: FixtureSteerOutcome,
    control_path: PathBuf,
    trace_path: PathBuf,
}

#[derive(Clone, Copy)]
enum FixtureSteerOutcome {
    Incorporated,
    Accepted,
    Rejected,
    Unknown,
}

impl FixtureSteerOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Incorporated => "incorporated",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum FixtureControlAction {
    Tool,
    Done,
}

impl FixtureControlAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureControlCommand {
    sequence: u64,
    action: FixtureControlAction,
}

struct FixtureLifecycleControl {
    command_path: PathBuf,
    trace_path: PathBuf,
    last_sequence: u64,
}

impl FixtureLifecycleControl {
    fn new(command_path: PathBuf, trace_path: PathBuf) -> Self {
        let last_sequence = read_control_command(&command_path)
            .map(|command| command.sequence)
            .unwrap_or_default();
        Self {
            command_path,
            trace_path,
            last_sequence,
        }
    }

    fn poll(&mut self) -> Option<FixtureControlCommand> {
        let command = read_control_command(&self.command_path)?;
        if command.sequence <= self.last_sequence {
            return None;
        }
        self.last_sequence = command.sequence;
        Some(command)
    }

    fn trace(&self, run_id: &str, event: &str, detail: serde_json::Value) {
        let Some(parent) = self.trace_path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.trace_path)
        else {
            return;
        };
        let line = serde_json::json!({
            "atMs": chrono::Utc::now().timestamp_millis(),
            "runId": run_id,
            "event": event,
            "detail": detail,
        });
        let _ = writeln!(file, "{line}");
    }
}

fn read_control_command(path: &Path) -> Option<FixtureControlCommand> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
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
        let safety_deadline = self.safety_deadline;
        let steer_outcome = self.steer_outcome;
        let control_path = self.control_path.clone();
        let trace_path = self.trace_path.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let session_id = format!("l09-qa-{}", uuid::Uuid::new_v4());
            let mut lifecycle = FixtureLifecycleControl::new(control_path, trace_path);
            lifecycle.trace(
                &session_id,
                "runStarted",
                serde_json::json!({
                    "baselineSequence": lifecycle.last_sequence,
                    "steerOutcome": steer_outcome.as_str(),
                    "safetyDeadlineMs": safety_deadline.as_millis(),
                }),
            );
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
            let interrupt = controls.interrupt;
            let mut steering = controls.steering;
            let deadline = tokio::time::sleep(safety_deadline);
            tokio::pin!(deadline);
            let mut control_poll = tokio::time::interval(Duration::from_millis(100));
            let mut steering_open = true;
            let mut tool_emitted = false;
            let status = loop {
                tokio::select! {
                    _ = interrupt.cancelled() => {
                        lifecycle.trace(&session_id, "interrupted", serde_json::json!({}));
                        break DoneStatus::Interrupted;
                    }
                    _ = &mut deadline => {
                        lifecycle.trace(&session_id, "safetyDeadline", serde_json::json!({}));
                        break DoneStatus::Completed;
                    }
                    _ = control_poll.tick() => {
                        let Some(command) = lifecycle.poll() else {
                            continue;
                        };
                        match command.action {
                            FixtureControlAction::Tool if !tool_emitted => {
                                tool_emitted = true;
                                lifecycle.trace(
                                    &session_id,
                                    "controlApplied",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "action": command.action.as_str(),
                                    }),
                                );
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
                            }
                            FixtureControlAction::Tool => {
                                lifecycle.trace(
                                    &session_id,
                                    "controlIgnored",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "action": command.action.as_str(),
                                        "reason": "toolAlreadyEmitted",
                                    }),
                                );
                            }
                            FixtureControlAction::Done => {
                                lifecycle.trace(
                                    &session_id,
                                    "controlApplied",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "action": command.action.as_str(),
                                    }),
                                );
                                break DoneStatus::Completed;
                            }
                        }
                    }
                    steer = steering.recv(), if steering_open => match steer {
                        Some(steer) => {
                            lifecycle.trace(
                                &session_id,
                                "steerReceived",
                                serde_json::json!({
                                    "messageId": steer.message_id,
                                    "delivery": format!("{:?}", steer.delivery),
                                    "outcome": steer_outcome.as_str(),
                                }),
                            );
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
            lifecycle.trace(
                &session_id,
                "doneEmitted",
                serde_json::json!({ "status": format!("{status:?}") }),
            );
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

fn fixture_safety_deadline() -> Duration {
    Duration::from_millis(
        std::env::var("L09_QA_SAFETY_MS")
            .or_else(|_| std::env::var("L09_QA_MOCK_HOLD_MS"))
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(600_000)
            .clamp(5_000, 900_000),
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
        safety_deadline: fixture_safety_deadline(),
        steer_outcome: fixture_steer_outcome(),
        control_path: root.join("l09-fixture-control.json"),
        trace_path: root.join("l09-fixture-control-trace.jsonl"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_control(path: &Path, sequence: u64, action: &str) {
        std::fs::write(
            path,
            serde_json::json!({ "sequence": sequence, "action": action }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn fixture_startup_loads_saved_settings_and_fresh_roots_default() {
        let fresh = tempfile::tempdir().unwrap();
        assert_eq!(
            settings::UiSettings::load(fresh.path()).follow_up_behavior,
            zeron_proto::FollowUpBehavior::Queue
        );

        let persisted = tempfile::tempdir().unwrap();
        let mut saved = settings::UiSettings::default();
        saved.follow_up_behavior = zeron_proto::FollowUpBehavior::Steer;
        std::fs::write(
            persisted.path().join("ui-settings.json"),
            serde_json::to_vec_pretty(&saved).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings::UiSettings::load(persisted.path()).follow_up_behavior,
            zeron_proto::FollowUpBehavior::Steer
        );
    }

    #[test]
    fn fixture_control_ignores_baseline_and_consumes_each_sequence_once() {
        let root = tempfile::tempdir().unwrap();
        let command = root.path().join("control.json");
        let trace = root.path().join("trace.jsonl");
        write_control(&command, 7, "tool");
        let mut lifecycle = FixtureLifecycleControl::new(command.clone(), trace);
        assert!(
            lifecycle.poll().is_none(),
            "baseline command must not replay"
        );

        write_control(&command, 8, "tool");
        let tool = lifecycle.poll().expect("new tool command");
        assert_eq!(tool.sequence, 8);
        assert_eq!(tool.action, FixtureControlAction::Tool);
        assert!(lifecycle.poll().is_none(), "same sequence is once-only");

        write_control(&command, 9, "done");
        let done = lifecycle.poll().expect("new done command");
        assert_eq!(done.sequence, 9);
        assert_eq!(done.action, FixtureControlAction::Done);
        assert!(lifecycle.poll().is_none(), "done cannot replay");
    }

    #[tokio::test]
    async fn fixture_control_drives_one_tool_boundary_then_done_without_eof() {
        let root = tempfile::tempdir().unwrap();
        let command = root.path().join("control.json");
        let trace = root.path().join("trace.jsonl");
        write_control(&command, 1, "done");
        let harness = L09FixtureHarness {
            safety_deadline: Duration::from_secs(5),
            steer_outcome: FixtureSteerOutcome::Incorporated,
            control_path: command.clone(),
            trace_path: trace.clone(),
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::channel(1);
        let controls = RunControls {
            request_input: Box::new(|_| {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(Vec::new());
                rx
            }),
            steering: steering_rx,
            interrupt: zeron_harness::CancellationToken::new(),
        };
        let request = RunRequest {
            prompt: "fixture control test".into(),
            harness: Some(HarnessId::Mock),
            provider_account_id: None,
            model: Some("mock-1".into()),
            reasoning: None,
            model_options: serde_json::Map::new(),
            cwd: root.path().display().to_string(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            permission_mode: PermissionMode::Ask,
            parent_permission_ceiling: None,
            environment_permission_ceiling: None,
            permission_policy: None,
            resume: None,
            attachments: Vec::new(),
            worktree: None,
        };
        let mut stream = harness.run(request, controls).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            AgentEvent::SessionStarted { .. }
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            AgentEvent::TextDelta { .. }
        ));

        write_control(&command, 2, "tool");
        let tool = tokio::time::timeout(Duration::from_secs(2), async {
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                let complete = matches!(event, AgentEvent::ToolResult { .. });
                events.push(event);
                if complete {
                    break;
                }
            }
            events
        })
        .await
        .unwrap();
        assert_eq!(
            tool.iter()
                .filter(|event| matches!(event, AgentEvent::ToolCall { .. }))
                .count(),
            1
        );
        assert_eq!(
            tool.iter()
                .filter(|event| matches!(event, AgentEvent::ToolResult { .. }))
                .count(),
            1
        );

        write_control(&command, 3, "tool");
        tokio::time::sleep(Duration::from_millis(200)).await;
        write_control(&command, 4, "done");
        drop(steering_tx);
        let tail = tokio::time::timeout(Duration::from_secs(2), async {
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event.unwrap());
            }
            events
        })
        .await
        .unwrap();
        assert_eq!(
            tail.iter()
                .filter(|event| matches!(event, AgentEvent::ToolCall { .. }))
                .count(),
            0,
            "the second tool command is once-only ignored"
        );
        assert_eq!(
            tail.iter()
                .filter(|event| matches!(event, AgentEvent::Done { .. }))
                .count(),
            1
        );
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains("toolAlreadyEmitted"));
        assert_eq!(trace.matches("doneEmitted").count(), 1);
    }
}
