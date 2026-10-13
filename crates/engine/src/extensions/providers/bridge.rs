//! The provider bridge: a `Harness` that serves one package's provider
//! contribution through CORE-07 supervised instances and provider profile
//! 1.0. It translates; it never invents acceptance, authority or completion.
//!
//! - Every model discovery and every run gets its own instance, stopped when
//!   it ends, so a crash, a Stop or a hang affects only that run.
//! - A run is admitted against the reviewed declaration before any process
//!   starts. Only a worker's `rejected` answer to `run.start` (or a failure
//!   before it) is "rejected before acceptance"; anything else after the
//!   start was sent is delivery-unknown and never resent.
//! - Events are long-polled with a cursor: duplicates are dropped by `seq`,
//!   gaps are conformance failures. Only the worker's terminal `done`
//!   completes a run; every other ending is `Errored` or `Interrupted`.
//! - Stop asks the worker, waits briefly for its `done{interrupted}`, and
//!   then stops the instance regardless, which ends every process in the
//!   worker's group (the CLI included).
//! - Approvals are settled here by permission mode and never reach a person
//!   in Auto or Full (docs/pending-tasks/permission-modes).

use std::sync::{Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_harness::{CancellationToken, Harness, HarnessError, RunControls, SteerAcceptance};
use luna_proto::contract::ExtensionId;
use luna_proto::{
    AgentEvent, CallExtensionInstanceRequest, DoneStatus, ExtensionCallStatus,
    ExtensionRuntimeKind, ExtensionWorkerExit, HarnessId, Model, PermissionMode, ProviderNotice,
    ProviderNoticeKind, ReasoningLevel, RunRequest, StartExtensionInstanceRequest, SteeringMode,
    ToolCall, UserInputAnswer, UserInputQuestion,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use super::super::{ExtensionManager, Inner};
use super::declaration::{OptionalProfile, PROFILE_MAJOR, ProfileVersion, ProviderDeclaration};

const DESCRIBE: &str = "provider.describe";
const MODELS: &str = "provider.models";
const RUN_START: &str = "provider.run.start";
const RUN_EVENTS: &str = "provider.run.events";
const RUN_STOP: &str = "provider.run.stop";
const RUN_ANSWER: &str = "provider.run.answer";
/// Provider profile 1.0's mandatory operations, required at every start.
const MANDATORY: [&str; 5] = [DESCRIBE, MODELS, RUN_START, RUN_EVENTS, RUN_STOP];

const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one `run.events` call may wait for events.
const POLL_WAIT: Duration = Duration::from_secs(10);
/// Allowance above the wait before the supervisor cancels the call.
const POLL_SLACK: Duration = Duration::from_secs(5);
const STOP_CALL_TIMEOUT: Duration = Duration::from_millis(1500);
const STOP_POLL_WAIT: Duration = Duration::from_millis(300);
/// How long a stop may take before LUNA ends the worker's group itself. The
/// engine settles an interrupted turn after 3 s, so this stays below it.
const STOP_CONFIRM: Duration = Duration::from_secs(2);
const BUSY_RETRY: Duration = Duration::from_millis(100);
/// Consecutive unanswered event requests that fail the run.
const MAX_DEADLINE_STREAK: u32 = 2;

const MAX_MODELS: usize = 256;
const MAX_ID_BYTES: usize = 256;
const MAX_LABEL_CHARS: usize = 128;
const MAX_EVENTS_PER_PAGE: usize = 2048;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_QUESTIONS: usize = 16;
const MAX_EVIDENCE_BYTES: usize = 2 * 1024;
const STREAM_BUFFER: usize = 256;

/// The last `provider.describe` this provider answered, for readback.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DescribeRecord {
    pub at_ms: i64,
    /// The worker described this provider in a profile this LUNA speaks.
    pub conforms: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileVersion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<Executable>,
    /// Why it is unavailable, or how it failed to conform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Executable {
    pub path: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Deserialize)]
struct WireVersion {
    major: u32,
    #[serde(default)]
    minor: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Described {
    profile: WireVersion,
    provider_id: String,
    available: bool,
    #[serde(default)]
    unavailable_reason: Option<String>,
    #[serde(default)]
    executable: Option<Executable>,
}

enum DescribeError {
    /// The provider reports it cannot run here (its CLI is missing).
    Unavailable(String),
    /// The worker broke the profile, or did not answer.
    Conformance(String),
}

impl DescribeError {
    fn into_message(self) -> String {
        match self {
            Self::Unavailable(message) | Self::Conformance(message) => message,
        }
    }
}

/// One supervised instance used by one discovery or one run.
#[derive(Clone)]
struct Session {
    manager: ExtensionManager,
    instance_id: String,
    generation: u32,
}

enum Reply {
    Ok(Value),
    Failed {
        code: String,
        message: String,
    },
    Deadline,
    /// The worker exited, or a replacement worker answered.
    Exited(Option<ExtensionWorkerExit>),
    Busy,
}

impl Session {
    /// `Err` means the engine refused the call: nothing reached the worker.
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Reply, String> {
        let request: CallExtensionInstanceRequest = serde_json::from_value(json!({
            "instanceId": self.instance_id,
            "method": method,
            "params": params,
            "timeoutMs": timeout.as_millis() as u64,
        }))
        .map_err(|error| format!("the {method} call could not be formed: {error}"))?;
        let outcome = self.manager.call_instance(request).await?;
        if outcome.generation != self.generation {
            // The supervisor restarted a crashed worker for this call; the
            // run's worker is gone and nothing is replayed.
            return Ok(Reply::Exited(outcome.exit));
        }
        Ok(match outcome.status {
            ExtensionCallStatus::Ok => Reply::Ok(outcome.result.unwrap_or(Value::Null)),
            ExtensionCallStatus::Failed => {
                let error = outcome
                    .error
                    .unwrap_or_else(|| luna_proto::ExtensionCallError {
                        code: "failed".into(),
                        message: String::new(),
                    });
                Reply::Failed {
                    code: error.code,
                    message: error.message,
                }
            }
            ExtensionCallStatus::DeadlineExceeded => Reply::Deadline,
            ExtensionCallStatus::Busy => Reply::Busy,
            // `workerExited`, and any later status: the answer is unknown.
            _ => Reply::Exited(outcome.exit),
        })
    }

    async fn close(&self) {
        if let Err(error) = self.manager.stop_instance(&self.instance_id).await {
            tracing::debug!(instance = %self.instance_id, %error, "provider instance already gone");
        }
    }
}

pub(super) struct ExtensionProvider {
    id: HarnessId,
    extension_id: ExtensionId,
    version: String,
    package_hash: String,
    runtime: ExtensionRuntimeKind,
    declaration: ProviderDeclaration,
    manager: Weak<Inner>,
    /// The exact package this provider runs; part of catalog provenance.
    source: String,
    last_describe: Mutex<Option<DescribeRecord>>,
}

impl ExtensionProvider {
    pub(super) fn new(
        id: HarnessId,
        extension_id: ExtensionId,
        version: String,
        package_hash: String,
        runtime: ExtensionRuntimeKind,
        declaration: ProviderDeclaration,
        manager: Weak<Inner>,
    ) -> Self {
        let source = format!(
            "extension:{}@{version}#{package_hash}",
            extension_id.as_str()
        );
        Self {
            id,
            extension_id,
            version,
            package_hash,
            runtime,
            declaration,
            manager,
            source,
            last_describe: Mutex::new(None),
        }
    }

    pub(super) fn extension_id(&self) -> &ExtensionId {
        &self.extension_id
    }

    pub(super) fn version(&self) -> &str {
        &self.version
    }

    pub(super) fn runtime_kind(&self) -> ExtensionRuntimeKind {
        self.runtime
    }

    pub(super) fn last_describe(&self) -> Option<DescribeRecord> {
        self.last_describe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn label(&self) -> &str {
        &self.declaration.display_name
    }

    fn manager(&self) -> Option<ExtensionManager> {
        self.manager.upgrade().map(ExtensionManager)
    }

    fn required_operations(&self) -> Vec<String> {
        let mut operations: Vec<String> = MANDATORY.iter().map(|op| (*op).to_owned()).collect();
        if self.declaration.needs_answer_operation() {
            operations.push(RUN_ANSWER.to_owned());
        }
        operations
    }

    /// Start a verified instance that advertises every required operation.
    async fn open_session(&self) -> Result<Session, String> {
        let manager = self
            .manager()
            .ok_or_else(|| format!("{}: the engine is shutting down", self.label()))?;
        let request: StartExtensionInstanceRequest = serde_json::from_value(json!({
            "extensionId": self.extension_id,
            "packageHash": self.package_hash,
            "require": self.required_operations(),
        }))
        .map_err(|error| format!("the provider start could not be formed: {error}"))?;
        let view = manager
            .start_instance(request)
            .await
            .map_err(|error| format!("{} could not start its worker: {error}", self.label()))?;
        Ok(Session {
            manager,
            instance_id: view.instance_id,
            generation: view.generation,
        })
    }

    fn call_failure(&self, method: &str, reply: Reply) -> String {
        let label = self.label();
        match reply {
            Reply::Ok(_) => format!("{label}'s worker answered {method} unexpectedly"),
            Reply::Failed { code, message } => {
                format!("{label}'s worker refused {method} ({code}): {message}")
            }
            Reply::Deadline => format!("{label}'s worker did not answer {method} in time"),
            Reply::Exited(exit) => {
                format!(
                    "{label}'s worker exited during {method}{}",
                    exit_detail(exit.as_ref())
                )
            }
            Reply::Busy => format!("{label}'s worker was busy and did not take {method}"),
        }
    }

    /// `provider.describe`, checked against the declaration and recorded.
    async fn describe(&self, session: &Session) -> Result<(), DescribeError> {
        let reply = session.call(DESCRIBE, json!({}), DESCRIBE_TIMEOUT).await;
        let mut record = DescribeRecord {
            at_ms: chrono::Utc::now().timestamp_millis(),
            conforms: false,
            available: None,
            profile: None,
            executable: None,
            detail: None,
        };
        let result = match reply {
            Err(error) => Err(DescribeError::Conformance(error)),
            Ok(Reply::Ok(value)) => match serde_json::from_value::<Described>(value) {
                Err(error) => Err(DescribeError::Conformance(format!(
                    "{}'s description is invalid: {error}",
                    self.label()
                ))),
                Ok(described) => {
                    record.profile = Some(ProfileVersion {
                        major: described.profile.major,
                        minor: described.profile.minor,
                    });
                    record.available = Some(described.available);
                    record.executable = described.executable.clone();
                    self.check_description(described)
                }
            },
            Ok(reply) => Err(DescribeError::Conformance(
                self.call_failure(DESCRIBE, reply),
            )),
        };
        match &result {
            Ok(()) => record.conforms = true,
            Err(DescribeError::Unavailable(message)) => {
                record.conforms = true;
                record.detail = Some(message.clone());
            }
            Err(DescribeError::Conformance(message)) => record.detail = Some(message.clone()),
        }
        *self
            .last_describe
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(record);
        result
    }

    fn check_description(&self, described: Described) -> Result<(), DescribeError> {
        let label = self.label();
        if described.profile.major != PROFILE_MAJOR {
            return Err(DescribeError::Conformance(format!(
                "{label}'s worker speaks provider profile {}.{}; this LUNA speaks {PROFILE_MAJOR}.x",
                described.profile.major, described.profile.minor
            )));
        }
        if described.provider_id != self.id.as_str() {
            return Err(DescribeError::Conformance(format!(
                "{label}'s worker describes provider {}, not {}",
                clip(described.provider_id, MAX_ID_BYTES),
                self.id
            )));
        }
        if !described.available {
            let reason = described
                .unavailable_reason
                .map(|reason| clip(reason, MAX_MESSAGE_BYTES))
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or_else(|| "it gave no reason".into());
            return Err(DescribeError::Unavailable(format!(
                "{label} is unavailable on this device: {reason}"
            )));
        }
        Ok(())
    }

    async fn discover(&self) -> Result<Vec<Model>, HarnessError> {
        let session = self.open_session().await.map_err(HarnessError::Protocol)?;
        let result = self.discover_with(&session).await;
        session.close().await;
        result
    }

    async fn discover_with(&self, session: &Session) -> Result<Vec<Model>, HarnessError> {
        match self.describe(session).await {
            Ok(()) => {}
            Err(DescribeError::Unavailable(message)) => {
                return Err(HarnessError::NotInstalled(message));
            }
            Err(DescribeError::Conformance(message)) => {
                return Err(HarnessError::Protocol(message));
            }
        }
        let value = match session.call(MODELS, json!({}), MODELS_TIMEOUT).await {
            Ok(Reply::Ok(value)) => value,
            Ok(reply) => return Err(HarnessError::Protocol(self.call_failure(MODELS, reply))),
            Err(error) => return Err(HarnessError::Protocol(error)),
        };
        self.check_models(value).map_err(HarnessError::Protocol)
    }

    fn check_models(&self, value: Value) -> Result<Vec<Model>, String> {
        #[derive(Deserialize)]
        struct ModelsReply {
            models: Vec<Model>,
        }
        let label = self.label();
        let reply: ModelsReply = serde_json::from_value(value)
            .map_err(|error| format!("{label}'s model list is invalid: {error}"))?;
        if reply.models.is_empty() {
            return Err(format!("{label} listed no models"));
        }
        if reply.models.len() > MAX_MODELS {
            return Err(format!("{label} listed more than {MAX_MODELS} models"));
        }
        let mut seen = std::collections::HashSet::new();
        let mut models = Vec::with_capacity(reply.models.len());
        for mut model in reply.models {
            if !valid_id(&model.id) || model.label.trim().is_empty() {
                return Err(format!(
                    "{label} listed a model without a valid id and label"
                ));
            }
            if !seen.insert(model.id.clone()) {
                return Err(format!("{label} listed model {} twice", model.id));
            }
            model.label = model.label.trim().chars().take(MAX_LABEL_CHARS).collect();
            model.description = model
                .description
                .map(|description| clip(description, MAX_MESSAGE_BYTES));
            model.first_seen_at_ms = None;
            models.push(model);
        }
        Ok(models)
    }

    /// Everything that can be refused before any process starts.
    fn admit(&self, request: &RunRequest, mode: PermissionMode) -> Result<(), String> {
        let label = self.label();
        let declaration = &self.declaration;
        if !declaration.supports_mode(mode) {
            let offered = if declaration.supports_mode(PermissionMode::Auto) {
                "Full access or Approve for me"
            } else {
                "Full access"
            };
            return Err(match mode {
                PermissionMode::Ask => {
                    format!("{label} does not offer legacy Ask; switch this chat to {offered}")
                }
                PermissionMode::Auto => format!(
                    "{label} has no automatic reviewer, so Approve for me is not offered; switch this chat to {offered}"
                ),
                PermissionMode::Full => {
                    format!("{label} does not offer Full access; switch this chat to {offered}")
                }
            });
        }
        if request.resume.is_some() && !declaration.supports(OptionalProfile::Resume) {
            return Err(format!(
                "{label} cannot continue a saved conversation, and this chat has one; start a new chat to use {label}"
            ));
        }
        if !request.attachments.is_empty() && !declaration.supports(OptionalProfile::Attachments) {
            return Err(format!(
                "{label} takes text prompts only; remove the attachments and send again"
            ));
        }
        if !request.input_items.is_empty() {
            return Err(format!(
                "{label} takes plain text; remove the selected skills or mentions and send again"
            ));
        }
        if request.review.is_some() {
            return Err(format!("{label} has no native review"));
        }
        if request.goal.is_some() {
            return Err(format!("{label} has no native goals"));
        }
        if request
            .model_options
            .get(luna_harness::LUNA_NO_FILE_EDITS_OPTION)
            .is_some_and(|value| value == &Value::Bool(true))
        {
            return Err(format!(
                "{label} cannot guarantee a run without file edits; choose a provider that can"
            ));
        }
        Ok(())
    }

    async fn start_run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let label = self.label().to_owned();
        let mode = request
            .permission_policy
            .as_ref()
            .map_or(request.permission_mode, |policy| policy.effective_mode);
        self.admit(&request, mode)
            .map_err(HarnessError::RejectedBeforeAcceptance)?;
        let session = self
            .open_session()
            .await
            .map_err(HarnessError::RejectedBeforeAcceptance)?;
        if let Err(error) = self.describe(&session).await {
            session.close().await;
            return Err(HarnessError::RejectedBeforeAcceptance(error.into_message()));
        }
        let params = json!({
            "prompt": request.prompt,
            "cwd": request.cwd,
            "model": request.model,
            "reasoning": request.reasoning,
            "modelOptions": provider_options(&request.model_options),
            "permissionMode": mode,
            "resume": request.resume,
            "attachments": request.attachments,
        });
        let run_id = match session.call(RUN_START, params, START_TIMEOUT).await {
            Ok(Reply::Ok(value)) => match value.get("runId").and_then(Value::as_str) {
                Some(run_id) if valid_id(run_id) => run_id.to_owned(),
                _ => {
                    session.close().await;
                    return Err(HarnessError::Protocol(format!(
                        "{label}'s worker answered run.start without a run id; the prompt may have reached it, so LUNA does not send it again"
                    )));
                }
            },
            Ok(Reply::Failed { code, message }) if code == "rejected" => {
                session.close().await;
                return Err(HarnessError::RejectedBeforeAcceptance(format!(
                    "{label}: {}",
                    clip(message, MAX_MESSAGE_BYTES)
                )));
            }
            Ok(Reply::Busy) => {
                session.close().await;
                return Err(HarnessError::RejectedBeforeAcceptance(format!(
                    "{label}'s worker was busy; nothing was sent"
                )));
            }
            Err(error) => {
                session.close().await;
                return Err(HarnessError::RejectedBeforeAcceptance(format!(
                    "{label}: {error}"
                )));
            }
            Ok(reply) => {
                let failure = self.call_failure(RUN_START, reply);
                session.close().await;
                return Err(HarnessError::Protocol(format!(
                    "{failure}; the prompt may have reached {label}, so LUNA does not send it again"
                )));
            }
        };

        let RunControls {
            request_input,
            mut steering,
            interrupt,
            invoke_luna_tool: _,
        } = controls;
        // Profile 1.0 takes no input during a turn; the engine sends none to
        // a provider without steering, and anything that arrives is refused.
        let steering_label = label.clone();
        let drain = tokio::spawn(async move {
            while let Some(message) = steering.recv().await {
                if let Some(acceptance) = message.acceptance {
                    let _ = acceptance.send(SteerAcceptance::Rejected(format!(
                        "{steering_label} cannot take a message during a turn; send it after the turn ends"
                    )));
                }
            }
        });
        let (tx, rx) = mpsc::channel(STREAM_BUFFER);
        let pump = Pump {
            id: self.id,
            label,
            declaration: self.declaration.clone(),
            session,
            run_id,
            mode,
            cwd: request.cwd,
            model: request.model,
            resume: request.resume,
            tx,
            request_input,
            interrupt,
            assistant_message_id: format!("ext-msg-{}", uuid::Uuid::new_v4()),
            cursor: 0,
            session_id: None,
            stop_requested_at: None,
            answers: Vec::new(),
        };
        tokio::spawn(async move {
            pump.run().await;
            drain.abort();
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .boxed())
    }
}

#[async_trait]
impl Harness for ExtensionProvider {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn display_name(&self) -> &str {
        &self.declaration.display_name
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &self.declaration.reasoning_levels
    }
    /// Its package is still the enabled one in this profile (a file read).
    fn installed(&self) -> bool {
        self.manager().is_some_and(|manager| {
            manager.provider_package_enabled(self.extension_id.as_str(), &self.package_hash)
        })
    }
    fn model_catalog_source(&self) -> String {
        self.source.clone()
    }
    /// Every run stream ends with exactly one `Done` from the bridge.
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    fn authoritative_prompt_end(&self) -> bool {
        true
    }
    /// A resumed run must echo the requested session before its output, or
    /// it is stopped; it never silently continues another session.
    fn supports_safe_recovery_continuation(&self) -> bool {
        self.declaration.supports(OptionalProfile::Resume)
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.discover().await
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.start_run(request, controls).await
    }
}

type InputRequester =
    Box<dyn Fn(Vec<UserInputQuestion>) -> oneshot::Receiver<Vec<UserInputAnswer>> + Send + Sync>;

/// Drives one accepted run until its single `Done`.
struct Pump {
    id: HarnessId,
    label: String,
    declaration: ProviderDeclaration,
    session: Session,
    run_id: String,
    mode: PermissionMode,
    cwd: String,
    model: Option<String>,
    resume: Option<String>,
    tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    request_input: InputRequester,
    interrupt: CancellationToken,
    assistant_message_id: String,
    /// The last event `seq` applied.
    cursor: u64,
    session_id: Option<String>,
    stop_requested_at: Option<Instant>,
    answers: Vec<tokio::task::JoinHandle<()>>,
}

enum Applied {
    Continue,
    Finished,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEvent {
    session_id: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct TextEvent {
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallEvent {
    id: String,
    name: String,
    #[serde(default)]
    call: Option<Value>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultEvent {
    id: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    output: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageEvent {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct MessageEvent {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireQuestion {
    id: String,
    #[serde(default)]
    header: String,
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    multi_select: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputRequestEvent {
    request_id: String,
    questions: Vec<WireQuestion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequestEvent {
    request_id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    subject: String,
}

#[derive(Deserialize)]
struct DoneEvent {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    result: Option<String>,
}

impl Pump {
    async fn run(mut self) {
        let mut deadline_streak = 0_u32;
        loop {
            if self.stop_requested_at.is_none()
                && (self.interrupt.is_cancelled() || self.tx.is_closed())
            {
                self.request_stop().await;
            }
            if self
                .stop_requested_at
                .is_some_and(|at| at.elapsed() >= STOP_CONFIRM)
            {
                return self.end_unconfirmed_stop().await;
            }
            let stopping = self.stop_requested_at.is_some();
            let wait = if stopping { STOP_POLL_WAIT } else { POLL_WAIT };
            let params = json!({
                "runId": self.run_id,
                "after": self.cursor,
                "waitMs": wait.as_millis() as u64,
            });
            let reply = {
                let poll = self.session.call(RUN_EVENTS, params, wait + POLL_SLACK);
                if stopping {
                    poll.await
                } else {
                    let interrupt = self.interrupt.clone();
                    let tx = self.tx.clone();
                    // Dropping the poll cancels it; the worker answers the
                    // cancel and the next iteration asks it to stop.
                    tokio::select! {
                        reply = poll => reply,
                        () = interrupt.cancelled() => continue,
                        () = tx.closed() => continue,
                    }
                }
            };
            match reply {
                Err(error) => {
                    let reason = format!("{}: {error}", self.label);
                    return self.fail(reason).await;
                }
                Ok(Reply::Ok(page)) => {
                    deadline_streak = 0;
                    match self.apply_page(page).await {
                        Ok(Applied::Finished) => return,
                        Ok(Applied::Continue) => {}
                        Err(reason) => return self.fail(reason).await,
                    }
                }
                Ok(Reply::Deadline) => {
                    deadline_streak += 1;
                    if deadline_streak >= MAX_DEADLINE_STREAK {
                        let reason = format!("{}'s worker stopped answering", self.label);
                        return self.fail(reason).await;
                    }
                }
                Ok(Reply::Busy) => tokio::time::sleep(BUSY_RETRY).await,
                Ok(Reply::Exited(exit)) => return self.worker_gone(exit).await,
                Ok(Reply::Failed { code, message }) => {
                    let reason = format!(
                        "{}'s worker refused its event request ({code}): {}",
                        self.label,
                        clip(message, MAX_MESSAGE_BYTES)
                    );
                    return self.fail(reason).await;
                }
            }
        }
    }

    async fn emit(&self, event: AgentEvent) {
        // A dropped stream is noticed by the loop, which then stops the run.
        let _ = self.tx.send(Ok(event)).await;
    }

    fn abort_answers(&mut self) {
        for task in self.answers.drain(..) {
            task.abort();
        }
    }

    async fn request_stop(&mut self) {
        self.stop_requested_at = Some(Instant::now());
        let params = json!({"runId": self.run_id});
        match self.session.call(RUN_STOP, params, STOP_CALL_TIMEOUT).await {
            Ok(Reply::Ok(_)) => {}
            Ok(_) | Err(_) => {
                tracing::debug!(provider = %self.id, "the provider did not acknowledge the stop")
            }
        }
    }

    /// No `done{interrupted}` in time: end the worker's group ourselves.
    async fn end_unconfirmed_stop(mut self) {
        self.session.close().await;
        self.abort_answers();
        let note = format!(
            "{} did not confirm the stop within {} s; LUNA stopped its worker and every process in its group",
            self.label,
            STOP_CONFIRM.as_secs()
        );
        self.emit(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: Some(note),
            session_id: self.session_id.clone(),
        })
        .await;
    }

    /// A conformance or delivery failure: the run ends `Errored`, the worker
    /// is asked to stop the CLI, and its group is stopped regardless.
    async fn fail(mut self, reason: String) {
        self.emit(AgentEvent::Error {
            message: reason.clone(),
        })
        .await;
        let params = json!({"runId": self.run_id});
        let _ = self.session.call(RUN_STOP, params, STOP_CALL_TIMEOUT).await;
        self.session.close().await;
        self.abort_answers();
        self.emit(AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some(reason),
            session_id: self.session_id.clone(),
        })
        .await;
    }

    /// The worker exited (or was replaced) before its terminal event.
    async fn worker_gone(mut self, exit: Option<ExtensionWorkerExit>) {
        self.session.close().await;
        self.abort_answers();
        let detail = exit_detail(exit.as_ref());
        if self.stop_requested_at.is_some() {
            self.emit(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: Some(format!(
                    "{}'s worker exited while stopping{detail}",
                    self.label
                )),
                session_id: self.session_id.clone(),
            })
            .await;
            return;
        }
        let reason = format!(
            "{}'s worker exited during the run{detail}; the turn was not replayed",
            self.label
        );
        self.emit(AgentEvent::Error {
            message: reason.clone(),
        })
        .await;
        self.emit(AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some(reason),
            session_id: self.session_id.clone(),
        })
        .await;
    }

    async fn apply_page(&mut self, page: Value) -> Result<Applied, String> {
        let page: Page = serde_json::from_value(page)
            .map_err(|error| format!("{}'s events are invalid: {error}", self.label))?;
        if page.events.len() > MAX_EVENTS_PER_PAGE {
            return Err(format!(
                "{} sent more than {MAX_EVENTS_PER_PAGE} events at once",
                self.label
            ));
        }
        for raw in page.events {
            let seq = raw
                .get("seq")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("{} sent an event without a sequence number", self.label))?;
            if seq <= self.cursor {
                // Already applied: a repeated page after a lost reply.
                continue;
            }
            if seq != self.cursor + 1 {
                return Err(format!(
                    "{} skipped events {} to {}",
                    self.label,
                    self.cursor + 1,
                    seq - 1
                ));
            }
            self.cursor = seq;
            if let Applied::Finished = self.apply_event(raw).await? {
                return Ok(Applied::Finished);
            }
        }
        if page.done {
            return Err(format!(
                "{} reported its run done without a terminal event",
                self.label
            ));
        }
        Ok(Applied::Continue)
    }

    fn parse<T: DeserializeOwned>(&self, kind: &str, raw: Value) -> Result<T, String> {
        serde_json::from_value(raw)
            .map_err(|error| format!("{} sent an invalid {kind} event: {error}", self.label))
    }

    /// Output of a resumed run must follow its confirmed session.
    fn require_session(&self) -> Result<(), String> {
        if self.resume.is_some() && self.session_id.is_none() {
            return Err(format!(
                "{} did not confirm the resumed session before its output; LUNA stopped the run",
                self.label
            ));
        }
        Ok(())
    }

    async fn apply_event(&mut self, raw: Value) -> Result<Applied, String> {
        let kind = raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match kind.as_str() {
            "session" => {
                let event: SessionEvent = self.parse(&kind, raw)?;
                self.on_session(event).await?;
            }
            "text" => {
                let event: TextEvent = self.parse(&kind, raw)?;
                self.require_session()?;
                self.emit(AgentEvent::TextDelta {
                    text: clip(event.text, MAX_TEXT_BYTES),
                })
                .await;
            }
            "reasoning" => {
                let event: TextEvent = self.parse(&kind, raw)?;
                self.require_session()?;
                self.emit(AgentEvent::ReasoningDelta {
                    text: clip(event.text, MAX_TEXT_BYTES),
                })
                .await;
            }
            "toolCall" => {
                let event: ToolCallEvent = self.parse(&kind, raw)?;
                self.require_session()?;
                if !valid_id(&event.id) || !valid_id(&event.name) {
                    return Err(format!(
                        "{} sent a tool call without a valid id and name",
                        self.label
                    ));
                }
                let call = tool_call(event.name, event.call, event.input);
                self.emit(AgentEvent::ToolCall { id: event.id, call }).await;
            }
            "toolResult" => {
                let event: ToolResultEvent = self.parse(&kind, raw)?;
                self.require_session()?;
                if !valid_id(&event.id) {
                    return Err(format!(
                        "{} sent a tool result without a valid id",
                        self.label
                    ));
                }
                self.emit(AgentEvent::ToolResult {
                    id: event.id,
                    is_error: event.is_error,
                    output: event
                        .output
                        .map(|output| clip(output, MAX_TOOL_OUTPUT_BYTES)),
                    diff: None,
                })
                .await;
            }
            "usage" => {
                let event: UsageEvent = self.parse(&kind, raw)?;
                self.emit(AgentEvent::Usage {
                    input_tokens: event.input_tokens,
                    output_tokens: event.output_tokens,
                })
                .await;
            }
            "notice" => {
                let event: MessageEvent = self.parse(&kind, raw)?;
                self.emit(AgentEvent::ProviderNotice {
                    notice: ProviderNotice {
                        kind: ProviderNoticeKind::Warning,
                        message: clip(event.message, MAX_MESSAGE_BYTES),
                        at_ms: chrono::Utc::now().timestamp_millis(),
                    },
                })
                .await;
            }
            "error" => {
                let event: MessageEvent = self.parse(&kind, raw)?;
                self.emit(AgentEvent::Error {
                    message: clip(event.message, MAX_MESSAGE_BYTES),
                })
                .await;
            }
            "inputRequest" => {
                let event: InputRequestEvent = self.parse(&kind, raw)?;
                self.on_input_request(event)?;
            }
            "approvalRequest" => {
                let event: ApprovalRequestEvent = self.parse(&kind, raw)?;
                self.on_approval_request(event).await?;
            }
            "done" => {
                let event: DoneEvent = self.parse(&kind, raw)?;
                self.finish(event).await;
                return Ok(Applied::Finished);
            }
            _ => {
                // Recorded for inspection; never a transcript part.
                let mut payload = raw;
                if !serde_json::to_vec(&payload)
                    .is_ok_and(|bytes| bytes.len() <= MAX_EVIDENCE_BYTES)
                {
                    payload = json!({"truncated": true});
                }
                self.emit(AgentEvent::ProviderEvidence {
                    provider: self.id,
                    kind: format!("unknown:{}", clip(kind.clone(), 64)),
                    native_id: None,
                    payload,
                })
                .await;
            }
        }
        Ok(Applied::Continue)
    }

    async fn on_session(&mut self, event: SessionEvent) -> Result<(), String> {
        if !valid_id(&event.session_id) {
            return Err(format!("{} reported an invalid session id", self.label));
        }
        if let Some(current) = &self.session_id {
            if *current != event.session_id {
                return Err(format!(
                    "{} switched sessions during the run; LUNA stopped it",
                    self.label
                ));
            }
            return Ok(());
        }
        if let Some(resume) = &self.resume
            && *resume != event.session_id
        {
            return Err(format!(
                "{} continued session {} instead of this chat's session {resume}; LUNA stopped the run so the conversation is not split",
                self.label, event.session_id
            ));
        }
        self.session_id = Some(event.session_id.clone());
        let model = event
            .model
            .filter(|model| valid_id(model))
            .or_else(|| self.model.clone())
            .unwrap_or_default();
        self.emit(AgentEvent::SessionStarted {
            harness: self.id,
            model,
            tools: Vec::new(),
            cwd: self.cwd.clone(),
            session_id: event.session_id,
            assistant_message_id: self.assistant_message_id.clone(),
        })
        .await;
        Ok(())
    }

    /// The `questions` profile: a content question reaches the user in every
    /// mode, through the engine's own input request.
    fn on_input_request(&mut self, event: InputRequestEvent) -> Result<(), String> {
        if !self.declaration.supports(OptionalProfile::Questions) {
            return Err(format!(
                "{} asked a question without declaring the questions profile; LUNA stopped the run",
                self.label
            ));
        }
        if !valid_id(&event.request_id)
            || event.questions.is_empty()
            || event.questions.len() > MAX_QUESTIONS
        {
            return Err(format!("{} sent an invalid question request", self.label));
        }
        let mut questions = Vec::with_capacity(event.questions.len());
        for question in event.questions {
            if !valid_id(&question.id) || question.question.trim().is_empty() {
                return Err(format!("{} sent an invalid question", self.label));
            }
            questions.push(UserInputQuestion {
                id: question.id,
                header: clip(question.header, MAX_LABEL_CHARS),
                question: clip(question.question, MAX_MESSAGE_BYTES),
                options: question
                    .options
                    .into_iter()
                    .take(32)
                    .map(|option| clip(option, MAX_LABEL_CHARS))
                    .collect(),
                multi_select: question.multi_select,
                approval: None,
                mcp: None,
                input: None,
            });
        }
        let answered = (self.request_input)(questions);
        let session = self.session.clone();
        let run_id = self.run_id.clone();
        let request_id = event.request_id;
        self.answers.push(tokio::spawn(async move {
            let params = match answered.await {
                Ok(answers) if !answers.is_empty() => {
                    json!({"runId": run_id, "requestId": request_id, "answers": answers})
                }
                // Dismissed, or the run is stopping.
                _ => json!({"runId": run_id, "requestId": request_id, "cancelled": true}),
            };
            let _ = session.call(RUN_ANSWER, params, ANSWER_TIMEOUT).await;
        }));
        Ok(())
    }

    /// The `approvals` profile, settled by mode. Full allows this one request;
    /// Auto means the provider's own reviewer handed it back, so it is denied
    /// with the reason shown. Neither ever becomes a card.
    async fn on_approval_request(&mut self, event: ApprovalRequestEvent) -> Result<(), String> {
        if !self.declaration.supports(OptionalProfile::Approvals) {
            return Err(format!(
                "{} sent an approval request without declaring the approvals profile; LUNA stopped the run",
                self.label
            ));
        }
        if !valid_id(&event.request_id) {
            return Err(format!("{} sent an invalid approval request", self.label));
        }
        let subject = if event.subject.trim().is_empty() {
            if event.kind.trim().is_empty() {
                "an action".to_owned()
            } else {
                clip(event.kind, 64)
            }
        } else {
            clip(event.subject, MAX_LABEL_CHARS)
        };
        let params = if self.mode == PermissionMode::Full {
            json!({"runId": self.run_id, "requestId": event.request_id, "decision": "allow"})
        } else {
            let reason = format!(
                "Auto access declined {subject}. It needed a person's approval, and Auto never asks, so it did not run. Switch this chat to Full access to allow it."
            );
            self.emit(AgentEvent::Error {
                message: reason.clone(),
            })
            .await;
            json!({
                "runId": self.run_id,
                "requestId": event.request_id,
                "decision": "deny",
                "reason": reason,
            })
        };
        let session = self.session.clone();
        self.answers.push(tokio::spawn(async move {
            let _ = session.call(RUN_ANSWER, params, ANSWER_TIMEOUT).await;
        }));
        Ok(())
    }

    /// The worker's terminal event; the only way a run completes.
    async fn finish(&mut self, done: DoneEvent) {
        let label = &self.label;
        let (status, error) = match done.status.as_str() {
            "completed" => (DoneStatus::Completed, None),
            "interrupted" if self.stop_requested_at.is_some() => (DoneStatus::Interrupted, None),
            "interrupted" => (
                DoneStatus::Errored,
                Some(format!(
                    "{label} reported an interruption LUNA did not ask for"
                )),
            ),
            "failed" => (
                DoneStatus::Errored,
                Some(
                    done.error
                        .map(|error| clip(error, MAX_MESSAGE_BYTES))
                        .filter(|error| !error.trim().is_empty())
                        .unwrap_or_else(|| format!("{label} failed without a reason")),
                ),
            ),
            other => (
                DoneStatus::Errored,
                Some(format!(
                    "{label} ended its run with an unknown status {:?}",
                    clip(other.to_owned(), 64)
                )),
            ),
        };
        if let Some(message) = &error {
            self.emit(AgentEvent::Error {
                message: message.clone(),
            })
            .await;
        }
        // Stop the worker before reporting the end, so a completed turn never
        // leaves its process behind.
        self.session.close().await;
        self.abort_answers();
        let result = if status == DoneStatus::Completed {
            done.result.map(|result| clip(result, MAX_TEXT_BYTES))
        } else {
            None
        };
        self.emit(AgentEvent::Done {
            status,
            result,
            error,
            session_id: self.session_id.clone(),
        })
        .await;
    }
}

/// The selections a provider may see: engine-private run options are not
/// provider choices.
fn provider_options(options: &Map<String, Value>) -> Map<String, Value> {
    options
        .iter()
        .filter(|(key, _)| !key.starts_with("luna") && !key.starts_with("__luna"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn tool_call(name: String, call: Option<Value>, input: Option<Value>) -> ToolCall {
    if let Some(shape) = &call {
        let field = |key: &str| {
            shape
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| clip(value.to_owned(), MAX_TOOL_INPUT_BYTES))
        };
        let mapped = match shape.get("kind").and_then(Value::as_str) {
            Some("exec") => field("command").map(|command| ToolCall::Exec { command }),
            Some("read") => field("path").map(|path| ToolCall::ReadFile { path }),
            Some("write") => field("path").map(|path| ToolCall::WriteFile {
                path,
                content: None,
            }),
            Some("edit") => field("path").map(|path| ToolCall::EditFile {
                path,
                old_string: None,
                new_string: None,
            }),
            Some("search") => field("pattern").map(|pattern| ToolCall::Search {
                pattern,
                path: field("path"),
            }),
            Some("glob") => field("pattern").map(|pattern| ToolCall::Glob { pattern }),
            Some("webFetch") => field("url").map(|url| ToolCall::WebFetch { url, prompt: None }),
            Some("webSearch") => field("query").map(|query| ToolCall::WebSearch { query }),
            _ => None,
        };
        if let Some(mapped) = mapped {
            return mapped;
        }
    }
    let input = input.filter(|input| {
        serde_json::to_vec(input).is_ok_and(|bytes| bytes.len() <= MAX_TOOL_INPUT_BYTES)
    });
    ToolCall::Unknown { name, input }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control)
}

/// At most `max` bytes, cut on a character boundary.
fn clip(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

fn exit_detail(exit: Option<&ExtensionWorkerExit>) -> String {
    let Some(exit) = exit else {
        return String::new();
    };
    let reason = serde_json::to_value(exit.reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "exited".into());
    let mut detail = format!(" ({reason}");
    if let Some(code) = exit.code {
        detail.push_str(&format!(", exit code {code}"));
    }
    if let Some(signal) = exit.signal {
        detail.push_str(&format!(", signal {signal}"));
    }
    if let Some(text) = &exit.detail {
        detail.push_str(&format!(": {}", clip(text.clone(), 256)));
    }
    detail.push(')');
    detail
}
