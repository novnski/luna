//! Interactive L08 composer-draft QA fixture.
//!
//! The data root is an explicit command-line argument. Provider homes are
//! synthetic paths below it, the fixture-only mock harness is selected, Auth
//! uses an in-memory credential broker rooted below the fixture, and native
//! provider/Keychain fallbacks are disabled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{fs::OpenOptions, io::Write as _};

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::{ContextCompactRequest, Harness, HarnessError, RunControls, SteerAcceptance};
use luna_proto::{
    AgentEvent, ChatConfig, ComposerDraft, ContextOperationStatus, DoneStatus, DraftBlob,
    DraftBlobState, DraftContext, HarnessId, Model, PermissionMode, ReasoningLevel, RunRequest,
    SandboxLevel, SteeringMode, TaskLineage, ToolCall,
};
use luna_rpc::RpcService as _;
use luna_rpc::methods;
use luna_ui::*;
use sha2::{Digest, Sha256};

const TASK_A: &str = "l08-native-a";
const TASK_B: &str = "l08-native-b";
const L11_TASK_A: &str = "11111111-1111-4111-8111-111111111111";
const L11_TASK_B: &str = "22222222-2222-4222-8222-222222222222";
const L11_TASK_C: &str = "33333333-3333-4333-8333-333333333333";
const L11_BOOTSTRAP_MARKER: &str = "l11-bootstrap-v2.complete";
const L11_RICH_DRAFT_TEXT: &str = "L11 rich draft survives clear, restart, and provider handoff";
const COMPOSER_CHIP_SAMPLE: &str =
    "/app-creator [src](luna-file:src/) [codex.rs](luna-file:codex.rs)";
const MEDIUM_REASONING: &[ReasoningLevel] = &[ReasoningLevel::Medium];
const L11_REASONING: &[ReasoningLevel] = &[ReasoningLevel::Medium, ReasoningLevel::High];
const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

struct L09FixtureHarness {
    generation: &'static str,
    harness_id: HarnessId,
    display_name: &'static str,
    model_id: &'static str,
    alternate_model_id: Option<&'static str>,
    reasoning_levels: &'static [ReasoningLevel],
    safety_deadline: Duration,
    steer_outcome: FixtureSteerOutcome,
    control_path: PathBuf,
    trace_path: PathBuf,
    compact_control: Option<L11CompactControl>,
    automatic_context_control: bool,
}

#[derive(Clone)]
struct L11CompactControl {
    command_path: PathBuf,
    trace_path: PathBuf,
    safety_deadline: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum L11CompactOutcome {
    Running,
    Completed,
    Unknown,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct L11CompactCommand {
    sequence: u64,
    outcome: L11CompactOutcome,
}

struct L11CompactPollGuard(Arc<AtomicBool>);

impl Drop for L11CompactPollGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
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
    FailOverload,
    FailSubscription,
    AutoContextRunning,
    AutoContextCompleted,
    AutoContextUnknown,
}

impl FixtureControlAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Done => "done",
            Self::FailOverload => "failOverload",
            Self::FailSubscription => "failSubscription",
            Self::AutoContextRunning => "autoContextRunning",
            Self::AutoContextCompleted => "autoContextCompleted",
            Self::AutoContextUnknown => "autoContextUnknown",
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureControlCommand {
    sequence: u64,
    action: FixtureControlAction,
    #[serde(default)]
    reset_at_ms: Option<i64>,
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

fn read_l11_compact_command(path: &Path) -> Option<L11CompactCommand> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

impl L11CompactControl {
    async fn compact(
        &self,
        harness: HarnessId,
        request: &ContextCompactRequest,
    ) -> Result<(), HarnessError> {
        let last_sequence = read_l11_compact_command(&self.command_path)
            .map(|command| command.sequence)
            .unwrap_or_default();
        let trace =
            FixtureLifecycleControl::new(self.command_path.clone(), self.trace_path.clone());
        trace.trace(
            &request.session_id,
            "compactStarted",
            serde_json::json!({
                "harness": harness,
                "baselineSequence": last_sequence,
                "cwd": request.cwd,
                "model": request.model,
            }),
        );
        let running = Arc::new(AtomicBool::new(true));
        let guard = L11CompactPollGuard(running.clone());
        let command_path = self.command_path.clone();
        let trace_path = self.trace_path.clone();
        let session_id = request.session_id.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("l11-compact-control".into())
            .spawn(move || {
                let trace = FixtureLifecycleControl::new(command_path.clone(), trace_path);
                let mut last_sequence = last_sequence;
                while running.load(Ordering::Acquire) {
                    if let Some(command) = read_l11_compact_command(&command_path)
                        && command.sequence > last_sequence
                    {
                        last_sequence = command.sequence;
                        match command.outcome {
                            L11CompactOutcome::Running => trace.trace(
                                &session_id,
                                "compactRunning",
                                serde_json::json!({ "sequence": command.sequence }),
                            ),
                            L11CompactOutcome::Completed => {
                                trace.trace(
                                    &session_id,
                                    "compactCompleted",
                                    serde_json::json!({ "sequence": command.sequence }),
                                );
                                let _ = result_tx.send(Ok(()));
                                return;
                            }
                            L11CompactOutcome::Unknown => {
                                trace.trace(
                                    &session_id,
                                    "compactUnknown",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "reason": "controlledUnknown",
                                    }),
                                );
                                let _ =
                                    result_tx
                                        .send(Err("synthetic L11 compact completion is unknown"
                                            .to_string()));
                                return;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })?;
        let result = tokio::time::timeout(self.safety_deadline, result_rx).await;
        drop(guard);
        match result {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(HarnessError::Protocol(error)),
            Ok(Err(_)) => Err(HarnessError::Protocol(
                "synthetic L11 compact control stopped before readback".into(),
            )),
            Err(_) => {
                trace.trace(
                    &request.session_id,
                    "compactUnknown",
                    serde_json::json!({ "reason": "safetyDeadline" }),
                );
                Err(HarnessError::Protocol(
                    "synthetic L11 compact completion is unknown".into(),
                ))
            }
        }
    }
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
        self.harness_id
    }

    fn display_name(&self) -> &str {
        self.display_name
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        self.reasoning_levels
    }

    fn deterministic_turn_end(&self) -> bool {
        true
    }

    fn supports_safe_recovery_continuation(&self) -> bool {
        true
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        let mut models = vec![Model {
            id: self.model_id.into(),
            label: format!("{} · primary synthetic", self.display_name),
            description: Some("bounded fixture-only synthetic harness".into()),
            reasoning_levels: self.reasoning_levels.to_vec(),
            options: Vec::new(),
        }];
        if let Some(alternate) = self.alternate_model_id {
            models.push(Model {
                id: alternate.into(),
                label: format!("{} · alternate synthetic", self.display_name),
                description: Some("second synthetic model for same-provider QA".into()),
                reasoning_levels: self.reasoning_levels.to_vec(),
                options: Vec::new(),
            });
        }
        Ok(models)
    }

    async fn compact_context(&self, request: ContextCompactRequest) -> Result<(), HarnessError> {
        let Some(control) = self.compact_control.as_ref() else {
            return Err(HarnessError::Protocol(
                "fixture generation does not expose compaction".into(),
            ));
        };
        control.compact(self.harness_id, &request).await
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let safety_deadline = self.safety_deadline;
        let steer_outcome = self.steer_outcome;
        let generation = self.generation;
        let harness_id = self.harness_id;
        let model_id = self.model_id;
        let requested_model = request
            .model
            .clone()
            .unwrap_or_else(|| model_id.to_string());
        let control_path = self.control_path.clone();
        let trace_path = self.trace_path.clone();
        let automatic_context_control = self.automatic_context_control;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let session_id = request.resume.clone().unwrap_or_else(|| {
                format!(
                    "{}-qa-{}",
                    generation.to_ascii_lowercase(),
                    uuid::Uuid::new_v4()
                )
            });
            let mut lifecycle = FixtureLifecycleControl::new(control_path, trace_path);
            lifecycle.trace(
                &session_id,
                "runStarted",
                serde_json::json!({
                    "baselineSequence": lifecycle.last_sequence,
                    "steerOutcome": steer_outcome.as_str(),
                    "safetyDeadlineMs": safety_deadline.as_millis(),
                    "prompt": request.prompt.clone(),
                    "attachments": request.attachments.clone(),
                    "resume": request.resume.clone(),
                    "harness": request.harness,
                    "model": request.model.clone(),
                    "modelOptions": request.model_options.clone(),
                    "cwd": request.cwd.clone(),
                    "sandbox": request.sandbox,
                    "permissionMode": request.permission_mode,
                }),
            );
            let mut assistant_message_id = format!(
                "{}-qa-assistant-{}",
                generation.to_ascii_lowercase(),
                uuid::Uuid::new_v4()
            );
            let _ = tx.send(Ok(AgentEvent::SessionStarted {
                harness: harness_id,
                model: requested_model,
                tools: Vec::new(),
                cwd: request.cwd,
                session_id: session_id.clone(),
                assistant_message_id: assistant_message_id.clone(),
            }));
            let _ = tx.send(Ok(AgentEvent::TextDelta {
                text: format!(
                    "{generation} isolated synthetic turn accepted. No provider was contacted."
                ),
            }));
            if generation == "L11" && !automatic_context_control {
                let _ = tx.send(Ok(AgentEvent::ContextUsage {
                    tokens: Some(42_000),
                    window: Some(200_000),
                }));
                lifecycle.trace(
                    &session_id,
                    "syntheticTurnCompleted",
                    serde_json::json!({ "harness": harness_id }),
                );
                let _ = tx.send(Ok(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some(session_id),
                }));
                return;
            }
            let interrupt = controls.interrupt;
            let mut steering = controls.steering;
            let deadline = tokio::time::sleep(safety_deadline);
            tokio::pin!(deadline);
            let mut control_poll = tokio::time::interval(Duration::from_millis(100));
            let mut steering_open = true;
            let mut tool_emitted = false;
            let mut active_context_operation: Option<String> = None;
            let (status, done_error) = loop {
                tokio::select! {
                    _ = interrupt.cancelled() => {
                        lifecycle.trace(&session_id, "interrupted", serde_json::json!({}));
                        break (DoneStatus::Interrupted, None);
                    }
                    _ = &mut deadline => {
                        lifecycle.trace(&session_id, "safetyDeadline", serde_json::json!({}));
                        break (DoneStatus::Completed, None);
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
                                break (DoneStatus::Completed, None);
                            }
                            FixtureControlAction::FailOverload => {
                                let message = "synthetic provider overloaded".to_string();
                                lifecycle.trace(
                                    &session_id,
                                    "controlApplied",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "action": command.action.as_str(),
                                    }),
                                );
                                break (DoneStatus::Errored, Some(message));
                            }
                            FixtureControlAction::FailSubscription => {
                                let reset_at_ms = command.reset_at_ms.unwrap_or_else(|| {
                                    chrono::Utc::now().timestamp_millis() + 10 * 60 * 1_000
                                });
                                let message = format!(
                                    "synthetic subscription window exhausted reset_at_ms={reset_at_ms}"
                                );
                                lifecycle.trace(
                                    &session_id,
                                    "controlApplied",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "action": command.action.as_str(),
                                        "resetAtMs": reset_at_ms,
                                    }),
                                );
                                break (DoneStatus::Errored, Some(message));
                            }
                            FixtureControlAction::AutoContextRunning
                                if automatic_context_control
                                    && active_context_operation.is_none() =>
                            {
                                let operation_id = format!(
                                    "codex-auto:{session_id}:fixture-turn:fixture-compact-{}",
                                    command.sequence
                                );
                                active_context_operation = Some(operation_id.clone());
                                lifecycle.trace(
                                    &session_id,
                                    "automaticContextRunning",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "operationId": operation_id,
                                        "syntheticOnly": true,
                                    }),
                                );
                                let _ = tx.send(Ok(AgentEvent::ContextCompaction {
                                    operation_id,
                                    status: ContextOperationStatus::Running,
                                    thread_id: session_id.clone(),
                                    turn_id: "fixture-turn".into(),
                                }));
                            }
                            FixtureControlAction::AutoContextCompleted
                            | FixtureControlAction::AutoContextUnknown
                                if automatic_context_control
                                    && active_context_operation.is_some() =>
                            {
                                let operation_id = active_context_operation
                                    .take()
                                    .expect("checked synthetic context operation");
                                let status = if command.action
                                    == FixtureControlAction::AutoContextCompleted
                                {
                                    ContextOperationStatus::Completed
                                } else {
                                    ContextOperationStatus::Unknown
                                };
                                lifecycle.trace(
                                    &session_id,
                                    if status == ContextOperationStatus::Completed {
                                        "automaticContextCompleted"
                                    } else {
                                        "automaticContextUnknown"
                                    },
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "operationId": operation_id,
                                        "syntheticOnly": true,
                                    }),
                                );
                                let _ = tx.send(Ok(AgentEvent::ContextCompaction {
                                    operation_id,
                                    status,
                                    thread_id: session_id.clone(),
                                    turn_id: "fixture-turn".into(),
                                }));
                            }
                            FixtureControlAction::AutoContextRunning
                            | FixtureControlAction::AutoContextCompleted
                            | FixtureControlAction::AutoContextUnknown => {
                                lifecycle.trace(
                                    &session_id,
                                    "controlIgnored",
                                    serde_json::json!({
                                        "sequence": command.sequence,
                                        "action": command.action.as_str(),
                                        "reason": if automatic_context_control {
                                            "automaticContextStateMismatch"
                                        } else {
                                            "automaticContextUnsupportedForHarness"
                                        },
                                    }),
                                );
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
                                        "{}-qa-assistant-{}",
                                        generation.to_ascii_lowercase(),
                                        uuid::Uuid::new_v4()
                                    );
                                    let _ = tx.send(Ok(AgentEvent::Steered {
                                        user_message_id: None,
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
                error: done_error,
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
        std::env::var("L11_QA_SAFETY_MS")
            .or_else(|_| std::env::var("L10_QA_SAFETY_MS"))
            .or_else(|_| std::env::var("L09_QA_SAFETY_MS"))
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
        window.set_rem_size(px(typography::font_size(cx).pixels()));
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
    }
}

fn draft(task_id: &str, revision: u64, text: &str) -> ComposerDraft {
    ComposerDraft {
        input_items: Vec::new(),
        version: luna_proto::COMPOSER_DRAFT_VERSION,
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
    let client = luna_rpc::memory_client(core.rpc_service());
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
    let client = luna_rpc::memory_client(core.rpc_service());
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
        let receipt: luna_proto::CommandReceipt = client
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
            receipt.status == luna_proto::CommandReceiptStatus::Applied,
            "fixture queue seed was not applied"
        );
    }
    Ok(())
}

async fn seed_turn_recovery(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let client = luna_rpc::memory_client(core.rpc_service());
    let project = turn_recovery_project(root);
    std::fs::create_dir_all(&project)?;
    core.doc_host.open(TASK_A)?;
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId":"l10-native-attachment",
                "data":PNG_B64,
                "seq":0
            }),
        )
        .await?;
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({
                "uploadId":"l10-native-attachment",
                "fileName":"l10-frozen.png"
            }),
        )
        .await?;
    let attachment = committed["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("fixture upload returned no path"))?;
    let request = turn_recovery_request(&project, attachment);
    ensure_turn_recovery_request_matches_chat(core, &request)?;
    let receipt: luna_proto::CommandReceipt = client
        .call_as(
            methods::EXECUTE_TASK_COMMAND,
            serde_json::json!({
                "commandId":"l10-native-original-command",
                "actorId":"l10-native-fixture",
                "clientId":"l10-native-fixture",
                "clientSessionId":"isolated",
                "taskId":TASK_A,
                    "intent": {
                        "kind":"session",
                        "command": {
                            "kind":"run",
                            "messageId":"l10-native-original-message",
                            "request": request
                        },
                        "transfers":[]
                    }
            }),
        )
        .await?;
    anyhow::ensure!(
        matches!(
            receipt.status,
            luna_proto::CommandReceiptStatus::Pending | luna_proto::CommandReceiptStatus::Applied
        ),
        "fixture recovery request was not accepted"
    );
    Ok(())
}

fn fixture_chat_config() -> ChatConfig {
    ChatConfig {
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
    }
}

fn l11_fixture_chat_config(harness: HarnessId, model: &str) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: Some(ReasoningLevel::High),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn l11_fixture_project(root: &Path) -> PathBuf {
    root.join("l11-project")
}

fn l11_run_request(project: &Path) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "L11 synthetic history seed with visible context usage".into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: Some("l11-mock-a".into()),
        reasoning: Some(ReasoningLevel::High),
        model_options: Default::default(),
        cwd: project.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

#[cfg(test)]
fn l11_codex_context_request(project: &Path) -> RunRequest {
    let mut request = l11_run_request(project);
    request.prompt = "L11 synthetic Codex automatic context lifecycle".into();
    request.harness = Some(HarnessId::Codex);
    request.model = Some("l11-codex-synthetic".into());
    request
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum L11SeedCheckpoint {
    Complete,
    AfterHistory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum L11SeedOutcome {
    Ready,
    AlreadyReady,
    PausedAfterHistory,
}

fn l11_command_receipt(
    core: &EngineCore,
    command_id: &str,
) -> anyhow::Result<Option<luna_proto::CommandReceipt>> {
    Ok(core.doc_host.task_command_receipt(command_id)?)
}

async fn wait_for_l11_applied_receipt(
    core: &EngineCore,
    command_id: &str,
) -> anyhow::Result<luna_proto::CommandReceipt> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(receipt) = l11_command_receipt(core, command_id)? {
                match receipt.status {
                    luna_proto::CommandReceiptStatus::Pending => {}
                    luna_proto::CommandReceiptStatus::Applied => return Ok(receipt),
                    status => {
                        anyhow::bail!(
                            "L11 fixture command {command_id} ended {status:?}: {:?}",
                            receipt.reason
                        );
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("L11 fixture command {command_id} did not settle"))?
}

async fn l11_rpc_call<T: serde::de::DeserializeOwned>(
    core: &EngineCore,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<T> {
    match core.rpc_service().handle(method, params).await? {
        luna_rpc::RpcReply::Value(value) => Ok(serde_json::from_value(value)?),
        luna_rpc::RpcReply::Stream(_) => anyhow::bail!("unexpected stream reply from {method}"),
    }
}

async fn execute_l11_fenced_command(
    core: &EngineCore,
    command_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<luna_proto::CommandReceipt> {
    execute_l11_task_fenced_command(core, L11_TASK_A, command_id, intent).await
}

async fn execute_l11_task_fenced_command(
    core: &EngineCore,
    task_id: &str,
    command_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<luna_proto::CommandReceipt> {
    let read: luna_proto::TaskCommandRead = l11_rpc_call(
        core,
        methods::GET_TASK_COMMAND_SNAPSHOT,
        serde_json::json!({ "taskId": task_id }),
    )
    .await?;
    let mut envelope = serde_json::json!({
        "commandId":command_id,
        "actorId":"l11-native-fixture",
        "clientId":"l11-native-fixture",
        "clientSessionId":"isolated",
        "taskId":task_id,
        "intent":intent,
    });
    if let Some(generation) = read.snapshot.binding_generation {
        envelope["expectedBindingGeneration"] = generation.into();
    }
    l11_rpc_call(core, methods::EXECUTE_TASK_COMMAND, envelope).await
}

async fn ensure_l11_history(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    const COMMAND: &str = "l11-native-history-command";
    match l11_command_receipt(core, COMMAND)? {
        Some(receipt) if receipt.status == luna_proto::CommandReceiptStatus::Applied => {}
        Some(receipt) if receipt.status == luna_proto::CommandReceiptStatus::Pending => {
            wait_for_l11_applied_receipt(core, COMMAND).await?;
        }
        Some(receipt) => anyhow::bail!(
            "L11 fixture history receipt is terminal {:?}: {:?}",
            receipt.status,
            receipt.reason
        ),
        None => {
            let receipt = execute_l11_fenced_command(
                core,
                COMMAND,
                serde_json::json!({
                    "kind":"session",
                    "command": {
                        "kind":"run",
                        "messageId":"l11-native-history-message",
                        "request":l11_run_request(&l11_fixture_project(root))
                    },
                    "transfers":[]
                }),
            )
            .await?;
            match receipt.status {
                luna_proto::CommandReceiptStatus::Applied => {}
                luna_proto::CommandReceiptStatus::Pending => {
                    wait_for_l11_applied_receipt(core, COMMAND).await?;
                }
                status => anyhow::bail!(
                    "L11 fixture history request ended {status:?}: {:?}",
                    receipt.reason
                ),
            }
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.sessions.active_turn_id(L11_TASK_A).is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("L11 fixture history turn did not finish"))?;
    Ok(())
}

async fn seed_l11_context_controls_at(
    core: &EngineCore,
    root: &Path,
    checkpoint: L11SeedCheckpoint,
) -> anyhow::Result<L11SeedOutcome> {
    repair_l11_pristine_draft_metadata(core)?;
    let marker = root.join(L11_BOOTSTRAP_MARKER);
    if marker.is_file() {
        return Ok(L11SeedOutcome::AlreadyReady);
    }
    ensure_l11_history(core, root).await?;
    if checkpoint == L11SeedCheckpoint::AfterHistory {
        return Ok(L11SeedOutcome::PausedAfterHistory);
    }

    core.doc_host.open(L11_TASK_A)?;
    core.doc_host.pause_all_queues();
    let existing_queue = core
        .doc_host
        .queued_message(L11_TASK_A, "l11-native-queue-message")?;
    let draft_read = core
        .store
        .read_composer_draft(&core.device_id, Some(L11_TASK_A), None)?;
    let mut attachment = existing_queue
        .as_ref()
        .and_then(|row| row.attachments.first().cloned())
        .or_else(|| {
            draft_read
                .draft
                .as_ref()
                .and_then(|draft| draft.saved_blob_paths().next().map(str::to_string))
        });
    if attachment.is_none() {
        core.uploads
            .append("l11-native-attachment", PNG_B64, Some(0))?;
        attachment = Some(
            core.uploads
                .commit("l11-native-attachment", "l11-queue.png")?,
        );
    }
    let attachment = attachment.expect("fixture attachment was resolved");

    if existing_queue.is_none() {
        const COMMAND: &str = "l11-native-queue-command-v2";
        anyhow::ensure!(
            l11_command_receipt(core, COMMAND)?.is_none(),
            "L11 fixture queue receipt exists without its queue row"
        );
        let queued = execute_l11_fenced_command(
            core,
            COMMAND,
            serde_json::json!({
                "kind":"queueMessage",
                "messageId":"l11-native-queue-message",
                "text":"L11 queued message survives clear and provider handoff",
                "attachments":[attachment.clone()],
                "holdForTurnEnd":true,
                "execution": {
                    "harness":"mock",
                    "providerAccountId":null,
                    "model":"l11-mock-a",
                    "reasoning":"high",
                    "modelOptions":{},
                    "sandbox":"workspace-write",
                    "permissionMode":"ask",
                    "effectivePermissionMode":"ask"
                }
            }),
        )
        .await?;
        anyhow::ensure!(
            queued.status == luna_proto::CommandReceiptStatus::Applied,
            "L11 fixture queue request ended {:?}: {:?}",
            queued.status,
            queued.reason
        );
    }

    if draft_read.draft.is_none() {
        let png = base64::engine::general_purpose::STANDARD.decode(PNG_B64)?;
        let mut rich_draft = draft(L11_TASK_A, 1, L11_RICH_DRAFT_TEXT);
        rich_draft.attachments.push(DraftBlob {
            attachment_id: "l11-native-draft-attachment".into(),
            name: "l11-queue.png".into(),
            path: Some(attachment),
            sha256: Some(format!("{:x}", Sha256::digest(&png))),
            size: Some(png.len() as u64),
            state: DraftBlobState::Saved,
            error: None,
        });
        let outcome = core
            .store
            .save_composer_draft(&core.device_id, &rich_draft)?;
        anyhow::ensure!(
            matches!(
                outcome,
                luna_proto::ComposerDraftSaveOutcome::Saved
                    | luna_proto::ComposerDraftSaveOutcome::Unchanged
            ),
            "L11 fixture draft was not saved: {outcome:?}"
        );
    }

    let ready_queue = core
        .doc_host
        .queued_message(L11_TASK_A, "l11-native-queue-message")?
        .ok_or_else(|| anyhow::anyhow!("L11 fixture queue row is missing after seed"))?;
    core.store.replace_queue_attachment_owners(
        &core.device_id,
        [(ready_queue.id, ready_queue.attachments)],
    )?;
    anyhow::ensure!(
        core.store
            .read_composer_draft(&core.device_id, Some(L11_TASK_A), None)?
            .draft
            .is_some(),
        "L11 fixture draft is missing after seed"
    );

    let temp = root.join(format!(".{L11_BOOTSTRAP_MARKER}.{}", uuid::Uuid::new_v4()));
    std::fs::write(&temp, b"ready\n")?;
    std::fs::rename(temp, marker)?;
    Ok(L11SeedOutcome::Ready)
}

fn repair_l11_pristine_draft_metadata(core: &EngineCore) -> anyhow::Result<()> {
    let Some(mut record) = core
        .store
        .read_composer_draft(&core.device_id, Some(L11_TASK_A), None)?
        .draft
    else {
        return Ok(());
    };
    let legacy_execution =
        serde_json::to_value(l11_fixture_chat_config(HarnessId::Mock, "l11-mock-a"))?;
    if record.revision != 1
        || record.text != L11_RICH_DRAFT_TEXT
        || record.execution != legacy_execution
    {
        return Ok(());
    }
    record.revision = 2;
    record.execution = serde_json::Value::Null;
    record.updated_at_ms = chrono::Utc::now().timestamp_millis();
    anyhow::ensure!(
        core.store.save_composer_draft(&core.device_id, &record)?
            == luna_proto::ComposerDraftSaveOutcome::Saved,
        "legacy L11 fixture draft metadata was not repaired"
    );
    Ok(())
}

async fn seed_l11_context_controls(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    seed_l11_context_controls_at(core, root, L11SeedCheckpoint::Complete).await?;
    Ok(())
}

const TURN_RECOVERY_SPACE: &str = "turn-recovery-project";

fn turn_recovery_project(root: &Path) -> PathBuf {
    root.join("fixture-project")
}

fn turn_recovery_request(project: &Path, attachment: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "L10 frozen request with one attachment".into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: Some("mock-1".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: project.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![attachment.into()],
        resume: None,
        worktree: None,
    }
}

fn ensure_turn_recovery_request_matches_chat(
    core: &EngineCore,
    request: &RunRequest,
) -> anyhow::Result<()> {
    let chat = core
        .workspace
        .chat(TASK_A)?
        .ok_or_else(|| anyhow::anyhow!("fixture task is missing"))?;
    let config = chat
        .config
        .ok_or_else(|| anyhow::anyhow!("fixture task configuration is missing"))?;
    anyhow::ensure!(
        chat.cwd.as_deref() == Some(request.cwd.as_str()),
        "fixture task cwd does not match the frozen request"
    );
    anyhow::ensure!(
        Some(config.harness) == request.harness,
        "fixture harness does not match the frozen request"
    );
    anyhow::ensure!(
        config.provider_account_id == request.provider_account_id,
        "fixture account does not match the frozen request"
    );
    anyhow::ensure!(
        config.model == request.model,
        "fixture model does not match the frozen request"
    );
    anyhow::ensure!(
        config.reasoning == request.reasoning,
        "fixture reasoning does not match the frozen request"
    );
    anyhow::ensure!(
        config.model_options == request.model_options,
        "fixture model options do not match the frozen request"
    );
    anyhow::ensure!(
        config.sandbox == request.sandbox,
        "fixture sandbox does not match the frozen request"
    );
    anyhow::ensure!(
        config.permission_mode == request.permission_mode,
        "fixture permission mode does not match the frozen request"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FixtureStartup {
    task_id: &'static str,
    seed_turn_recovery: bool,
    seed_l11_context: bool,
    stage_draft_evidence: bool,
}

fn initialize_fixture_workspace(
    core: &EngineCore,
    root: &Path,
    mode: &str,
) -> anyhow::Result<FixtureStartup> {
    let existing = core.workspace.read_chats()?;
    let l11 = mode == "context-controls";
    if l11 {
        let project = l11_fixture_project(root);
        std::fs::create_dir_all(&project)?;
        let cwd = project.to_string_lossy();
        let task_a_exists = existing.iter().any(|chat| chat.id == L11_TASK_A);
        let task_b_exists = existing.iter().any(|chat| chat.id == L11_TASK_B);
        let task_c_exists = existing.iter().any(|chat| chat.id == L11_TASK_C);
        core.task_identity.register_new_task(
            L11_TASK_A,
            None,
            Some(&core.device_id),
            Some(cwd.as_ref()),
            TaskLineage::default(),
        )?;
        core.task_identity.register_new_task(
            L11_TASK_C,
            None,
            Some(&core.device_id),
            Some(cwd.as_ref()),
            TaskLineage::default(),
        )?;
        core.task_identity.register_new_task(
            L11_TASK_B,
            None,
            Some(&core.device_id),
            Some(cwd.as_ref()),
            TaskLineage::default(),
        )?;
        if !task_a_exists {
            core.workspace.create_chat(
                L11_TASK_A,
                None,
                Some(&core.device_id),
                Some(l11_fixture_chat_config(HarnessId::Mock, "l11-mock-a")),
                Some(cwd.into_owned()),
            )?;
            core.workspace
                .rename_chat(L11_TASK_A, "L11 Context Controls")?;
        }
        if !task_b_exists {
            core.workspace.create_chat(
                L11_TASK_B,
                None,
                Some(&core.device_id),
                Some(l11_fixture_chat_config(HarnessId::Opencode, "l11-mock-b")),
                Some(project.to_string_lossy().into_owned()),
            )?;
            core.workspace
                .rename_chat(L11_TASK_B, "L11 Synthetic Handoff Target")?;
        }
        if !task_c_exists {
            core.workspace.create_chat(
                L11_TASK_C,
                None,
                Some(&core.device_id),
                Some(l11_fixture_chat_config(
                    HarnessId::Codex,
                    "l11-codex-synthetic",
                )),
                Some(project.to_string_lossy().into_owned()),
            )?;
            core.workspace
                .rename_chat(L11_TASK_C, "L11 Synthetic Codex Auto Context")?;
        }
        return Ok(FixtureStartup {
            task_id: L11_TASK_A,
            seed_turn_recovery: false,
            seed_l11_context: true,
            stage_draft_evidence: false,
        });
    }
    let task_a_exists = existing.iter().any(|chat| chat.id == TASK_A);
    let task_b_exists = existing.iter().any(|chat| chat.id == TASK_B);
    let turn_recovery = mode == "turn-recovery";
    let composer_project = mode.starts_with("composer-chips");
    if composer_project {
        core.workspace.create_space(
            "composer-preview-project",
            &core.device_id,
            root.to_str().unwrap(),
            Some("Synthetic composer project".into()),
            false,
        )?;
    }
    let project = turn_recovery_project(root);
    if turn_recovery {
        std::fs::create_dir_all(&project)?;
        // Sessions require a project: a projectless chat is hidden and cannot
        // be selected, which left this mode on an empty "No sessions" canvas.
        core.workspace.create_space(
            TURN_RECOVERY_SPACE,
            &core.device_id,
            project.to_str().unwrap(),
            Some("L10 recovery project".into()),
            false,
        )?;
    }
    let fixture_space = if turn_recovery {
        Some(TURN_RECOVERY_SPACE)
    } else {
        composer_project.then_some("composer-preview-project")
    };

    if !task_a_exists {
        core.workspace.create_chat(
            TASK_A,
            fixture_space,
            Some(&core.device_id),
            Some(fixture_chat_config()),
            turn_recovery
                .then(|| project.to_string_lossy().into_owned())
                .or_else(|| composer_project.then(|| root.to_string_lossy().into_owned())),
        )?;
        let title = match mode {
            "queue" => "L09 Durable Queue",
            "turn-recovery" => "L10 Failure Recovery",
            _ => "L08 Draft A",
        };
        core.workspace.rename_chat(TASK_A, title)?;
    }
    if !task_b_exists {
        core.workspace
            .create_chat(TASK_B, fixture_space, Some(&core.device_id), None, None)?;
        core.workspace.rename_chat(TASK_B, "L08 Draft B")?;
    }

    let first_turn_recovery_start = turn_recovery && !task_a_exists;
    Ok(FixtureStartup {
        task_id: TASK_A,
        seed_turn_recovery: first_turn_recovery_start,
        seed_l11_context: false,
        stage_draft_evidence: matches!(mode, "seed" | "queue") || first_turn_recovery_start,
    })
}

fn trace_turn_recovery_workspace(core: &EngineCore, root: &Path, first_run: bool) {
    let Ok(Some(chat)) = core.workspace.chat(TASK_A) else {
        return;
    };
    FixtureLifecycleControl::new(
        root.join("l10-fixture-control.json"),
        root.join("l10-fixture-control-trace.jsonl"),
    )
    .trace(
        "fixture-startup",
        "workspaceInitialized",
        serde_json::json!({
            "firstRun": first_run,
            "taskCwd": chat.cwd,
            "taskConfig": chat.config,
        }),
    );
}

fn register_l11_harnesses(registry: &HarnessRegistry, root: &Path) {
    let compact_control = L11CompactControl {
        command_path: root.join("l11-compact-control.json"),
        trace_path: root.join("l11-context-control-trace.jsonl"),
        safety_deadline: fixture_safety_deadline(),
    };
    for (harness_id, display_name, model_id, alternate_model_id, automatic_context_control) in [
        (
            HarnessId::Mock,
            "L11 QA Mock A",
            "l11-mock-a",
            "l11-mock-a-alt",
            false,
        ),
        (
            HarnessId::Opencode,
            "L11 QA Mock B · no executable",
            "l11-mock-b",
            "l11-mock-b-alt",
            false,
        ),
        (
            HarnessId::Codex,
            "L11 QA Synthetic Codex · no executable",
            "l11-codex-synthetic",
            "l11-codex-synthetic-alt",
            true,
        ),
    ] {
        registry.register(Arc::new(L09FixtureHarness {
            generation: "L11",
            harness_id,
            display_name,
            model_id,
            alternate_model_id: Some(alternate_model_id),
            reasoning_levels: L11_REASONING,
            safety_deadline: fixture_safety_deadline(),
            steer_outcome: FixtureSteerOutcome::Incorporated,
            control_path: root.join("l11-fixture-control.json"),
            trace_path: root.join("l11-context-control-trace.jsonl"),
            compact_control: (!automatic_context_control).then(|| compact_control.clone()),
            automatic_context_control,
        }));
    }
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
    let l10 = mode == "turn-recovery";
    let l11 = mode == "context-controls";
    if l11 {
        register_l11_harnesses(&registry, &root);
    } else {
        registry.register(Arc::new(L09FixtureHarness {
            generation: if l10 { "L10" } else { "L09" },
            harness_id: HarnessId::Mock,
            display_name: if l10 { "L10 QA Mock" } else { "L09 QA Mock" },
            model_id: "mock-1",
            alternate_model_id: None,
            reasoning_levels: MEDIUM_REASONING,
            safety_deadline: fixture_safety_deadline(),
            steer_outcome: fixture_steer_outcome(),
            control_path: root.join(if l10 {
                "l10-fixture-control.json"
            } else {
                "l09-fixture-control.json"
            }),
            trace_path: root.join(if l10 {
                "l10-fixture-control-trace.jsonl"
            } else {
                "l09-fixture-control-trace.jsonl"
            }),
            compact_control: None,
            automatic_context_control: false,
        }));
    }
    let core = runtime.block_on(async {
        let profile = EngineProfile::development(
            &engine_data,
            if l11 {
                "l11-native-org"
            } else {
                "l08-native-org"
            },
            if l11 {
                "l11-native-user"
            } else {
                "l08-native-user"
            },
        );
        if l11 {
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                profile,
                Arc::new(registry),
                HarnessId::Mock,
                None,
                accounts,
            )
        } else {
            EngineCore::assemble_with_profile_and_agent_accounts_config(
                profile,
                Arc::new(registry),
                HarnessId::Mock,
                None,
                accounts,
            )
        }
    })?;
    // The fixture deliberately separates engine stores from UI preferences;
    // point the recovery policy at the same isolated settings file the native
    // Settings page writes so the automatic-retries toggle is testable.
    core.sessions
        .set_recovery_settings_path(ui_data.join("ui-settings.json"));
    let mut auth_config = AuthConfig::new("http://127.0.0.1:1", root.join("auth"));
    auth_config.dev_user_id = if l11 {
        "l11-fixture-user".into()
    } else {
        "l09-fixture-user".into()
    };
    auth_config.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth_config));
    let startup = initialize_fixture_workspace(&core, &root, &mode)?;
    if matches!(
        mode.as_str(),
        "composer-chips-sent" | "composer-chips-sent-skill"
    ) {
        let text = if mode == "composer-chips-sent-skill" {
            "/app-creator"
        } else {
            COMPOSER_CHIP_SAMPLE
        };
        runtime.block_on(async {
            core.doc_host.open(startup.task_id)?.write_user_message(
                "composer-chips-sent-qa",
                text,
                chrono::Utc::now().timestamp_millis(),
            )?;
            Ok::<_, anyhow::Error>(())
        })?;
    }
    if mode == "recovery" {
        runtime.block_on(seed_recovery(&core))?;
    } else if mode == "queue" {
        runtime.block_on(seed_queue(&core))?;
    } else if mode == "turn-recovery" {
        trace_turn_recovery_workspace(&core, &root, startup.seed_turn_recovery);
        if startup.seed_turn_recovery {
            runtime.block_on(seed_turn_recovery(&core, &root))?;
        }
    } else if l11 && startup.seed_l11_context {
        runtime.block_on(seed_l11_context_controls(&core, &root))?;
    }

    let ipc_port = unused_port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(
        ipc_port,
        core.rpc_service(),
        &engine_data,
    ))?;
    let boot = EngineBootConfig {
        data_dir: engine_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    let device = core.device_id.clone();
    let seed = startup.stage_draft_evidence;
    let preview_chips = mode == "composer-chips";
    let selected_task_id = startup.task_id;

    // Engine teardown runs exactly once: from GPUI's quit hook on macOS, where
    // `NSApp terminate:` exits inside that hook and `run` never returns, or
    // after `run` returns on the other platforms.
    let core = Arc::new(core);
    let shutdown_engine: std::rc::Rc<dyn Fn()> = {
        let core = core.clone();
        let runtime = runtime.handle().clone();
        let once = std::sync::Once::new();
        std::rc::Rc::new(move || {
            once.call_once(|| {
                let _ = runtime.block_on(core.shutdown());
            })
        })
    };
    let shutdown_on_quit = shutdown_engine.clone();

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
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                // Use the production selection transition before the first frame.
                state.select_chat(Some(selected_task_id.into()), cx);
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
                    |window, cx| {
                        let shell = cx.new(|cx| shell::Shell::new(state.clone(), boot, cx));
                        // The app's main-window close guard: unsaved drafts or
                        // files keep the window open until they are saved.
                        let weak_shell = shell.downgrade();
                        window.on_window_should_close(cx, move |_, cx| {
                            let should_close = weak_shell
                                .update(cx, |shell, cx| shell.prepare_window_close(cx))
                                .unwrap_or(true);
                            if should_close {
                                settings::flush(cx);
                            }
                            should_close
                        });
                        shell
                    },
                )
                .unwrap();
            // A normal app lifecycle, so the fixture never strands the
            // one-GUI slot. The menu bar offers Quit (⌘Q, bound by
            // `Shell::new`'s keymap): with the window open it dispatches
            // `app_menus::Quit` to the global handler, whose `prepare_quit`
            // runs the save guard before terminating. Closing the last window
            // has already passed the window's close guard above, and GPUI's
            // own `LastWindowClosed` mode then quits once the window is
            // removed (an action dispatched from a window-closed observer was
            // lost: `active_window()` could still name the removed window).
            // On macOS that quit still goes through `native_quit`, which
            // re-runs `prepare_quit` (no Shell windows remain) before allowing
            // termination. The quit hook below shuts the engine down.
            cx.set_menus(app_menus::app_menus());
            cx.set_quit_mode(gpui::QuitMode::LastWindowClosed);
            cx.on_app_quit(move |_| {
                shutdown_on_quit();
                async {}
            })
            .detach();
            if seed {
                let composer = window
                    .update(cx, |shell, _, _| shell.fixture_appshots_composer())
                    .unwrap();
                composer.update(cx, |composer, cx| composer.fixture_stage_draft_evidence(cx));
            }
            if preview_chips {
                let composer = window
                    .update(cx, |shell, _, _| shell.fixture_appshots_composer())
                    .unwrap();
                composer.update(cx, |composer, cx| {
                    composer.fixture_stage_chip_preview(COMPOSER_CHIP_SAMPLE, cx)
                });
            }
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            if let Some(path) = capture_path {
                let exit_after_capture = std::env::var("LUNA_FIXTURE_EXIT_AFTER_CAPTURE")
                    .is_ok_and(|value| value == "1");
                cx.spawn(async move |cx| {
                    if let Err(error) = capture_after_load(window.into(), path, cx).await {
                        eprintln!("draft fixture capture failed: {error:#}");
                    }
                    if exit_after_capture {
                        cx.update(|cx| cx.quit());
                    }
                })
                .detach();
            }
        });
    shutdown_engine();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assemble_fixture_test_core(root: &Path) -> EngineCore {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&root.join("engine"), "fixture-org", "fixture-user"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            account_config(root),
        )
        .unwrap()
    }

    fn assemble_l11_fixture_test_core(root: &Path) -> EngineCore {
        let accounts = account_config(root);
        accounts.disable_claude_keychain_fallback().unwrap();
        let registry = HarnessRegistry::new();
        register_l11_harnesses(&registry, root);
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(
                    &root.join("engine"),
                    "l11-native-org",
                    "l11-native-user",
                ),
                Arc::new(registry),
                HarnessId::Mock,
                None,
                accounts,
            )
            .unwrap();
        let mut auth_config = AuthConfig::new("http://127.0.0.1:1", root.join("auth"));
        auth_config.dev_user_id = "l11-fixture-user".into();
        auth_config.use_in_memory_credentials_for_tests();
        core.set_auth(Auth::new(auth_config));
        core
    }

    fn l11_draft(core: &EngineCore) -> luna_proto::ComposerDraftRead {
        core.store
            .read_composer_draft(&core.device_id, Some(L11_TASK_A), None)
            .unwrap()
    }

    fn l11_history_seed_count(core: &EngineCore) -> usize {
        core.doc_host
            .open(L11_TASK_A)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .iter()
            .filter(|entry| {
                entry.role == luna_doc::MessageRole::User
                    && entry.id == "l11-native-history-message"
            })
            .count()
    }

    fn write_control(path: &Path, sequence: u64, action: &str) {
        std::fs::write(
            path,
            serde_json::json!({ "sequence": sequence, "action": action }).to_string(),
        )
        .unwrap();
    }

    fn write_control_atomic(path: &Path, sequence: u64, action: &str) {
        let temp = path.with_extension("json.tmp");
        std::fs::write(
            &temp,
            serde_json::json!({ "sequence": sequence, "action": action }).to_string(),
        )
        .unwrap();
        std::fs::rename(temp, path).unwrap();
    }

    fn write_l11_compact_control(path: &Path, sequence: u64, outcome: &str) {
        std::fs::write(
            path,
            serde_json::json!({ "sequence": sequence, "outcome": outcome }).to_string(),
        )
        .unwrap();
    }

    fn write_l11_compact_control_atomic(path: &Path, sequence: u64, outcome: &str) {
        let temp = path.with_extension("json.tmp");
        std::fs::write(
            &temp,
            serde_json::json!({ "sequence": sequence, "outcome": outcome }).to_string(),
        )
        .unwrap();
        std::fs::rename(temp, path).unwrap();
    }

    async fn wait_for_trace(path: &Path, event: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(path)
                    .ok()
                    .is_some_and(|trace| trace.contains(event))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("trace never recorded {event}"));
    }

    async fn wait_for_trace_count(path: &Path, event: &str, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(path)
                    .ok()
                    .is_some_and(|trace| trace.matches(event).count() >= count)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("trace never recorded {event} {count} times"));
    }

    #[test]
    fn fixture_startup_loads_saved_settings_and_fresh_roots_default() {
        let fresh = tempfile::tempdir().unwrap();
        assert_eq!(
            settings::UiSettings::load(fresh.path()).follow_up_behavior,
            luna_proto::FollowUpBehavior::Queue
        );

        let persisted = tempfile::tempdir().unwrap();
        let mut saved = settings::UiSettings::default();
        saved.follow_up_behavior = luna_proto::FollowUpBehavior::Steer;
        std::fs::write(
            persisted.path().join("ui-settings.json"),
            serde_json::to_vec_pretty(&saved).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings::UiSettings::load(persisted.path()).follow_up_behavior,
            luna_proto::FollowUpBehavior::Steer
        );
    }

    #[tokio::test]
    async fn l11_initialization_is_isolated_registered_and_restart_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_fixture_test_core(root.path());
        let first = initialize_fixture_workspace(&core, root.path(), "context-controls").unwrap();
        assert_eq!(first.task_id, L11_TASK_A);
        assert!(first.seed_l11_context);
        assert!(!first.stage_draft_evidence);
        assert!(core.task_identity.view(L11_TASK_A).is_some());
        assert!(core.task_identity.view(L11_TASK_B).is_some());
        assert!(core.task_identity.view(L11_TASK_C).is_some());
        let codex_chat = core.workspace.chat(L11_TASK_C).unwrap().unwrap();
        assert_eq!(codex_chat.config.unwrap().harness, HarnessId::Codex);
        let chat = core.workspace.chat(L11_TASK_A).unwrap().unwrap();
        let config = chat.config.unwrap();
        assert_eq!(config.harness, HarnessId::Mock);
        assert_eq!(config.model.as_deref(), Some("l11-mock-a"));
        assert_eq!(config.reasoning, Some(ReasoningLevel::High));
        assert!(
            chat.cwd
                .as_deref()
                .is_some_and(|cwd| cwd.starts_with(root.path().to_string_lossy().as_ref()))
        );

        let restart = initialize_fixture_workspace(&core, root.path(), "context-controls").unwrap();
        assert!(restart.seed_l11_context);
        assert!(!restart.stage_draft_evidence);
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn l11_async_seed_fresh_and_reopen_preserves_ready_state() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_l11_fixture_test_core(root.path());
        initialize_fixture_workspace(&core, root.path(), "context-controls").unwrap();
        assert_eq!(
            seed_l11_context_controls_at(&core, root.path(), L11SeedCheckpoint::Complete)
                .await
                .unwrap(),
            L11SeedOutcome::Ready
        );
        assert!(root.path().join(L11_BOOTSTRAP_MARKER).is_file());
        assert_eq!(l11_history_seed_count(&core), 1);
        assert_eq!(
            core.doc_host
                .task_command_receipt("l11-native-history-command")
                .unwrap()
                .unwrap()
                .status,
            luna_proto::CommandReceiptStatus::Applied
        );
        assert_eq!(
            core.doc_host
                .task_command_receipt("l11-native-queue-command-v2")
                .unwrap()
                .unwrap()
                .status,
            luna_proto::CommandReceiptStatus::Applied
        );
        let queue = core
            .doc_host
            .queued_message(L11_TASK_A, "l11-native-queue-message")
            .unwrap()
            .unwrap();
        assert_eq!(queue.attachments.len(), 1);
        assert!(std::path::Path::new(&queue.attachments[0]).is_file());
        let read = l11_draft(&core);
        let mut user_draft = read.draft.unwrap();
        assert!(user_draft.text.contains("L11 rich draft"));
        assert_eq!(user_draft.attachments.len(), 1);
        assert_eq!(user_draft.execution, serde_json::Value::Null);
        assert_eq!(
            core.store
                .save_composer_draft(&core.device_id, &user_draft)
                .unwrap(),
            luna_proto::ComposerDraftSaveOutcome::Unchanged,
            "the restored rich draft must pass the submit save gate at its existing revision"
        );
        assert_eq!(
            core.task_identity
                .view(L11_TASK_A)
                .unwrap()
                .current_binding
                .unwrap()
                .generation,
            1
        );
        let models = core
            .registry
            .resolve(HarnessId::Mock)
            .unwrap()
            .models()
            .await
            .unwrap();
        assert_eq!(models.len(), 2);
        assert!(
            models
                .iter()
                .all(|model| model.reasoning_levels.contains(&ReasoningLevel::High))
        );

        user_draft.revision += 1;
        user_draft.text = "user-edited L11 draft".into();
        let saved = core
            .store
            .save_composer_draft(&core.device_id, &user_draft)
            .unwrap();
        assert_eq!(saved, luna_proto::ComposerDraftSaveOutcome::Saved);
        let mut user_config = core
            .workspace
            .chat(L11_TASK_A)
            .unwrap()
            .unwrap()
            .config
            .unwrap();
        user_config.model = Some("l11-mock-a-alt".into());
        user_config.reasoning = Some(ReasoningLevel::Medium);
        core.workspace
            .set_chat_config(L11_TASK_A, &user_config)
            .unwrap();
        core.shutdown().await;
        drop(core);

        let db = root
            .path()
            .join("engine/orgs/l11-native-org/l11-native-user/docs.sqlite3");
        let output = std::process::Command::new("/usr/bin/sqlite3")
            .arg(db)
            .arg(format!(
                "SELECT COUNT(*) FROM attachment_owners WHERE owner_id IN ('l11-native-queue-message','{L11_TASK_A}')"
            ))
            .output()
            .unwrap();
        assert!(output.status.success());
        let owner_count: i64 = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(owner_count, 2);

        let reopened = assemble_l11_fixture_test_core(root.path());
        initialize_fixture_workspace(&reopened, root.path(), "context-controls").unwrap();
        assert_eq!(
            seed_l11_context_controls_at(&reopened, root.path(), L11SeedCheckpoint::Complete)
                .await
                .unwrap(),
            L11SeedOutcome::AlreadyReady
        );
        assert_eq!(l11_history_seed_count(&reopened), 1);
        assert_eq!(
            l11_draft(&reopened).draft.unwrap().text,
            "user-edited L11 draft"
        );
        let reopened_config = reopened
            .workspace
            .chat(L11_TASK_A)
            .unwrap()
            .unwrap()
            .config
            .unwrap();
        assert_eq!(reopened_config.model.as_deref(), Some("l11-mock-a-alt"));
        assert_eq!(reopened_config.reasoning, Some(ReasoningLevel::Medium));
        assert!(
            reopened
                .doc_host
                .queued_message(L11_TASK_A, "l11-native-queue-message")
                .unwrap()
                .is_some()
        );
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn l11_legacy_draft_metadata_repair_is_exact_and_once_only() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_fixture_test_core(root.path());
        let mut legacy = draft(L11_TASK_A, 1, L11_RICH_DRAFT_TEXT);
        legacy.execution =
            serde_json::to_value(l11_fixture_chat_config(HarnessId::Mock, "l11-mock-a")).unwrap();
        assert_eq!(
            core.store
                .save_composer_draft(&core.device_id, &legacy)
                .unwrap(),
            luna_proto::ComposerDraftSaveOutcome::Saved
        );
        repair_l11_pristine_draft_metadata(&core).unwrap();
        let mut repaired = l11_draft(&core).draft.unwrap();
        assert_eq!(repaired.revision, 2);
        assert_eq!(repaired.execution, serde_json::Value::Null);

        repaired.revision = 3;
        repaired.text = "user-edited L11 draft".into();
        repaired.execution = serde_json::json!({"user":"metadata"});
        core.store
            .save_composer_draft(&core.device_id, &repaired)
            .unwrap();
        repair_l11_pristine_draft_metadata(&core).unwrap();
        assert_eq!(l11_draft(&core).draft.unwrap(), repaired);
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn l11_async_seed_recovers_after_history_without_duplicate_turn() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_l11_fixture_test_core(root.path());
        initialize_fixture_workspace(&core, root.path(), "context-controls").unwrap();
        assert_eq!(
            seed_l11_context_controls_at(&core, root.path(), L11SeedCheckpoint::AfterHistory)
                .await
                .unwrap(),
            L11SeedOutcome::PausedAfterHistory
        );
        assert_eq!(l11_history_seed_count(&core), 1);
        assert!(!root.path().join(L11_BOOTSTRAP_MARKER).exists());
        let legacy_conflict: luna_proto::CommandReceipt = l11_rpc_call(
            &core,
            methods::EXECUTE_TASK_COMMAND,
            serde_json::json!({
                "commandId":"l11-native-queue-command",
                "actorId":"l11-native-fixture",
                "clientId":"l11-native-fixture",
                "clientSessionId":"isolated",
                "taskId":L11_TASK_A,
                "intent": {
                    "kind":"queueMessage",
                    "messageId":"l11-native-queue-message",
                    "text":"legacy unfenced fixture row",
                    "attachments":[],
                    "holdForTurnEnd":true
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            legacy_conflict.status,
            luna_proto::CommandReceiptStatus::Conflict
        );
        assert!(
            core.doc_host
                .queued_message(L11_TASK_A, "l11-native-queue-message")
                .unwrap()
                .is_none()
        );
        assert!(l11_draft(&core).draft.is_none());
        core.shutdown().await;
        drop(core);

        let reopened = assemble_l11_fixture_test_core(root.path());
        initialize_fixture_workspace(&reopened, root.path(), "context-controls").unwrap();
        assert_eq!(
            seed_l11_context_controls_at(&reopened, root.path(), L11SeedCheckpoint::Complete)
                .await
                .unwrap(),
            L11SeedOutcome::Ready
        );
        assert_eq!(l11_history_seed_count(&reopened), 1);
        assert!(
            reopened
                .doc_host
                .queued_message(L11_TASK_A, "l11-native-queue-message")
                .unwrap()
                .is_some()
        );
        assert!(l11_draft(&reopened).draft.is_some());
        let trace =
            std::fs::read_to_string(root.path().join("l11-context-control-trace.jsonl")).unwrap();
        assert_eq!(trace.matches("\"event\":\"runStarted\"").count(), 1);
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn l11_compact_control_exposes_running_completed_and_unknown() {
        let root = tempfile::tempdir().unwrap();
        let command = root.path().join("compact.json");
        let trace = root.path().join("trace.jsonl");
        write_l11_compact_control(&command, 1, "running");
        let control = L11CompactControl {
            command_path: command.clone(),
            trace_path: trace.clone(),
            safety_deadline: Duration::from_secs(2),
        };
        let request = ContextCompactRequest {
            session_id: "l11-session".into(),
            cwd: root.path().to_string_lossy().into_owned(),
            model: Some("l11-mock-a".into()),
        };
        let command_for_task = command.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            write_l11_compact_control(&command_for_task, 2, "running");
            tokio::time::sleep(Duration::from_millis(100)).await;
            write_l11_compact_control(&command_for_task, 3, "completed");
        });
        control.compact(HarnessId::Mock, &request).await.unwrap();
        let trace_text = std::fs::read_to_string(&trace).unwrap();
        assert!(trace_text.contains("compactRunning"));
        assert!(trace_text.contains("compactCompleted"));

        write_l11_compact_control(&command, 4, "completed");
        let command_for_task = command.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            write_l11_compact_control(&command_for_task, 5, "unknown");
        });
        let error = control
            .compact(HarnessId::Opencode, &request)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("completion is unknown"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn l11_synthetic_codex_controls_production_automatic_context_projection() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_l11_fixture_test_core(root.path());
        initialize_fixture_workspace(&core, root.path(), "context-controls").unwrap();
        let chat = core.workspace.chat(L11_TASK_C).unwrap().unwrap();
        assert_eq!(chat.config.as_ref().unwrap().harness, HarnessId::Codex);
        assert_eq!(
            chat.title.as_deref(),
            Some("L11 Synthetic Codex Auto Context")
        );

        let start = execute_l11_task_fenced_command(
            &core,
            L11_TASK_C,
            "l11-synthetic-codex-auto-completed",
            serde_json::json!({
                "kind":"session",
                "command": {
                    "kind":"run",
                    "messageId":"l11-synthetic-codex-auto-completed-message",
                    "request":l11_codex_context_request(&l11_fixture_project(root.path()))
                },
                "transfers":[]
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            start.status,
            luna_proto::CommandReceiptStatus::Pending | luna_proto::CommandReceiptStatus::Applied
        ));
        let trace = root.path().join("l11-context-control-trace.jsonl");
        let control = root.path().join("l11-fixture-control.json");
        wait_for_trace(&trace, "runStarted").await;
        write_control_atomic(&control, 1, "autoContextRunning");
        wait_for_trace(&trace, "automaticContextRunning").await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while core
                .doc_host
                .context_control(L11_TASK_C)
                .unwrap()
                .compact
                .status
                != ContextOperationStatus::Running
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        write_control_atomic(&control, 2, "autoContextCompleted");
        wait_for_trace(&trace, "automaticContextCompleted").await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while core
                .doc_host
                .context_control(L11_TASK_C)
                .unwrap()
                .compact
                .status
                != ContextOperationStatus::Completed
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        write_control_atomic(&control, 3, "done");
        tokio::time::timeout(Duration::from_secs(3), async {
            while core.sessions.has_live_run(L11_TASK_C) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        let second = execute_l11_task_fenced_command(
            &core,
            L11_TASK_C,
            "l11-synthetic-codex-auto-unknown",
            serde_json::json!({
                "kind":"session",
                "command": {
                    "kind":"run",
                    "messageId":"l11-synthetic-codex-auto-unknown-message",
                    "request":l11_codex_context_request(&l11_fixture_project(root.path()))
                },
                "transfers":[]
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            second.status,
            luna_proto::CommandReceiptStatus::Pending | luna_proto::CommandReceiptStatus::Applied
        ));
        wait_for_trace_count(&trace, "runStarted", 2).await;
        write_control_atomic(&control, 4, "autoContextRunning");
        tokio::time::timeout(Duration::from_secs(3), async {
            while core
                .doc_host
                .context_control(L11_TASK_C)
                .unwrap()
                .compact
                .status
                != ContextOperationStatus::Running
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        write_control_atomic(&control, 5, "autoContextUnknown");
        tokio::time::timeout(Duration::from_secs(3), async {
            while core
                .doc_host
                .context_control(L11_TASK_C)
                .unwrap()
                .compact
                .status
                != ContextOperationStatus::Unknown
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        write_control_atomic(&control, 6, "done");
        tokio::time::timeout(Duration::from_secs(3), async {
            while core.sessions.has_live_run(L11_TASK_C) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let view = core
            .task_identity
            .reconcile_task(L11_TASK_C, false)
            .await
            .unwrap();
        assert_eq!(
            view.current_binding.unwrap().reconciliation.state,
            luna_proto::ReconciliationState::Unsupported
        );
        assert!(
            core.task_identity
                .set_provider_name(L11_TASK_C, "must not reach provider metadata")
                .await
                .unwrap_err()
                .to_string()
                .contains("disabled for this synthetic runtime")
        );
        assert!(
            std::fs::read_to_string(&trace)
                .unwrap()
                .contains("\"syntheticOnly\":true")
        );
        core.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn l11_ipc_compact_consumes_script_control_and_applies_receipt() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_l11_fixture_test_core(root.path());
        initialize_fixture_workspace(&core, root.path(), "context-controls").unwrap();
        seed_l11_context_controls(&core, root.path()).await.unwrap();

        let port = unused_port();
        let ipc = luna_engine::serve_ipc(port, core.rpc_service(), &root.path().join("engine"))
            .await
            .unwrap();
        let client = luna_rpc::connect_ws(&format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let snapshot: luna_proto::TaskCommandRead = client
            .call_as(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                serde_json::json!({ "taskId": L11_TASK_A }),
            )
            .await
            .unwrap();
        let generation = snapshot.snapshot.binding_generation.unwrap();
        let compact = tokio::spawn(async move {
            client
                .call_as::<luna_proto::CommandReceipt>(
                    methods::EXECUTE_TASK_COMMAND,
                    serde_json::json!({
                        "commandId":"l11-native-ipc-compact",
                        "actorId":"l11-native-fixture-test",
                        "clientId":"l11-native-fixture-test",
                        "clientSessionId":"ipc",
                        "taskId":L11_TASK_A,
                        "expectedTaskRevision":snapshot.snapshot.task_revision,
                        "expectedBindingGeneration":generation,
                        "intent":{"kind":"compactContext"}
                    }),
                )
                .await
        });

        let trace = root.path().join("l11-context-control-trace.jsonl");
        let control = root.path().join("l11-compact-control.json");
        wait_for_trace(&trace, "compactStarted").await;
        write_l11_compact_control_atomic(&control, 1, "running");
        wait_for_trace(&trace, "compactRunning").await;
        write_l11_compact_control_atomic(&control, 2, "completed");
        let receipt = tokio::time::timeout(Duration::from_secs(5), compact)
            .await
            .expect("compact RPC stayed bounded")
            .expect("compact RPC task joined")
            .expect("compact RPC returned a receipt");
        assert_eq!(receipt.status, luna_proto::CommandReceiptStatus::Applied);
        assert_eq!(
            core.doc_host
                .context_control(L11_TASK_A)
                .unwrap()
                .compact
                .status,
            luna_proto::ContextOperationStatus::Completed
        );
        wait_for_trace(&trace, "compactCompleted").await;
        assert_eq!(
            std::fs::read_to_string(&control).unwrap(),
            serde_json::json!({"sequence":2,"outcome":"completed"}).to_string()
        );

        ipc.abort();
        let _ = ipc.await;
        core.shutdown().await;
    }

    #[tokio::test]
    async fn turn_recovery_initialization_matches_request_and_is_restart_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble_fixture_test_core(root.path());
        let first = initialize_fixture_workspace(&core, root.path(), "turn-recovery").unwrap();
        assert!(first.seed_turn_recovery);
        assert!(first.stage_draft_evidence);

        let request = turn_recovery_request(
            &turn_recovery_project(root.path()),
            "/synthetic/stable-frozen.png",
        );
        ensure_turn_recovery_request_matches_chat(&core, &request).unwrap();
        let original_chat = core.workspace.chat(TASK_A).unwrap().unwrap();
        assert_eq!(
            original_chat.cwd.as_deref(),
            Some(
                turn_recovery_project(root.path())
                    .to_string_lossy()
                    .as_ref()
            )
        );
        let mut runtime_config = original_chat.config.clone().unwrap();
        runtime_config
            .permission_audit
            .push(luna_proto::PermissionAuditEntry {
                actor: "engine:fixture".into(),
                machine: "fixture-host".into(),
                requested_mode: PermissionMode::Ask,
                effective_mode: PermissionMode::Ask,
                previous_mode: Some(PermissionMode::Ask),
                provider_mapping: "mock Ask".into(),
                ceiling_source: None,
                reason: None,
                timestamp_ms: 1,
            });
        core.workspace
            .set_chat_config(TASK_A, &runtime_config)
            .unwrap();
        let persisted_chat = core.workspace.chat(TASK_A).unwrap().unwrap();

        core.shutdown().await;
        drop(core);

        let reopened = assemble_fixture_test_core(root.path());
        let restart =
            initialize_fixture_workspace(&reopened, root.path(), "turn-recovery").unwrap();
        assert!(!restart.seed_turn_recovery);
        assert!(!restart.stage_draft_evidence);
        assert_eq!(
            reopened.workspace.chat(TASK_A).unwrap().unwrap(),
            persisted_chat,
            "reopen must not rewrite the task cwd, config, title, or durable session metadata"
        );
        reopened.shutdown().await;
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
            generation: "L09",
            harness_id: HarnessId::Mock,
            display_name: "L09 QA Mock",
            model_id: "mock-1",
            alternate_model_id: None,
            reasoning_levels: MEDIUM_REASONING,
            safety_deadline: Duration::from_secs(5),
            steer_outcome: FixtureSteerOutcome::Incorporated,
            control_path: command.clone(),
            trace_path: trace.clone(),
            compact_control: None,
            automatic_context_control: false,
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::channel(1);
        let controls = RunControls {
            request_input: Box::new(|_| {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(Vec::new());
                rx
            }),
            steering: steering_rx,
            interrupt: luna_harness::CancellationToken::new(),
            invoke_luna_tool: None,
        };
        let request = RunRequest {
            review: None,
            goal: None,
            input_items: Vec::new(),
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
