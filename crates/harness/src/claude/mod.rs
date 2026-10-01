//! Claude Code harness: spawns the installed `claude` CLI and speaks its
//! stream-json protocol directly — no adapter process in between. Resurrected
//! from the pre-ACP driver (see docs/research/harness.md) and modernized
//! against CLI 2.1.228.
//!
//! - stdout JSONL frames are normalized into [`AgentEvent`]s (init dedupe,
//!   subagent tagging, typed tool decoding, error-code mapping).
//! - PERMISSIONS ride the stdio control channel: `--permission-prompt-tool
//!   stdio` (undocumented — absent from `claude --help`, but it is the same
//!   transport the Claude Agent SDK's `query()` drives, and was re-validated
//!   live against 2.1.228: `can_use_tool` control requests arrive and
//!   allow/deny responses are honored). The alternative channel — an MCP
//!   permission tool — needs a server process and was rejected. Native
//!   acceptEdits/auto/bypassPermissions modes are selected from the resolved
//!   policy; escalations in Ask/Auto round-trip through the typed input bridge.
//! - DONE is the CLI's own `result` frame, eagerly: background work (a
//!   spawned subagent) never holds the turn. The CLI natively runs a second
//!   wake turn when a background task finishes — a fresh `init` (same
//!   session id, deduped) plus another `result` — and both are forwarded;
//!   the engine's parked-session resume path turns them into the
//!   done→Working→done wake.
//! - SUBAGENT frames arrive on the same stdout tagged with a top-level
//!   `parent_tool_use_id`; they are wrapped in [`AgentEvent::Subagent`] and
//!   NEVER folded into the parent feed (a background subagent interleaves
//!   with the parent's own stream — folding them in split contiguous text
//!   around phantom tool calls).
//! - Steering: queued [`SteerMessage`]s are written to stdin as user lines at
//!   any time; the CLI folds them into the running turn at its own step
//!   boundary.
//! - Interrupt: cancelling [`RunControls::interrupt`] sends the protocol-level
//!   interrupt control request, then escalates to SIGTERM and SIGKILL.

pub mod catalog;
mod controls;
mod discovery;
mod events;
mod normalize;
pub mod profile;
mod tool_results;
mod transport;
mod turns;
mod wire;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand,
    SteeringMode, UserInputAnswer, UserInputQuestion,
};

use crate::process::{Child, ChildStdin, Command, Stdio};
use crate::{
    CommandScope, Harness, HarnessError, RunControls, Signal, send_signal, shutdown_child,
};
use catalog::{apply_ultrathink, models_from_initialize, static_models, to_effort};
use normalize::Normalizer;
use profile::{ClaudeLaunchProfile, ToolSelection};
use wire::Frame;

/// Locate the device's installed Claude Code CLI: our own PATH, then the
/// login-shell PATH snapshot (the user's shell init shapes PATH in ways a
/// GUI/service launch never sees — see [`crate::shell_env`]), then known
/// install locations as a last resort. The `CLAUDE_CODE_EXECUTABLE` override
/// is applied by [`ClaudeHarness::resolve_executable`], so availability and
/// launches agree on one resolution order. Resolved per call — cheap after
/// the snapshot is cached.
pub fn resolve_claude_executable() -> Option<PathBuf> {
    let mut extra = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        extra.push(home.join(".claude").join("local").join("claude"));
        extra.push(home.join(".local").join("bin").join("claude"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin/claude"));
    extra.push(PathBuf::from("/usr/local/bin/claude"));
    crate::executable::find_on_paths("claude", extra)
}

fn option_is_on(options: &serde_json::Map<String, Value>, key: &str) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "on" || s == "true",
        _ => false,
    }
}

/// The Claude Code harness. Construct with [`ClaudeHarness::new`]; tests point
/// it at a fake CLI with [`ClaudeHarness::with_executable`].
pub struct ClaudeHarness {
    executable: Option<PathBuf>,
    /// Grace between the interrupt control request and SIGTERM.
    interrupt_grace: Duration,
    /// Grace between SIGTERM and SIGKILL.
    kill_grace: Duration,
    /// Explicit provider home (`CLAUDE_CONFIG_DIR`) for every child; `None`
    /// inherits the engine's environment.
    config_home: Option<PathBuf>,
    /// Scoped command discovery cache: only successful probes are cached, so a
    /// broken CLI retries on the next picker open (ACP-harness parity).
    discovery: discovery::DiscoveryCache,
}

impl Default for ClaudeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            config_home: None,
            discovery: discovery::DiscoveryCache::default(),
        }
    }
}

impl ClaudeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a fixed CLI binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Pin the provider home (`CLAUDE_CONFIG_DIR`) for runs AND discovery.
    pub fn with_config_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_home = Some(path.into());
        self
    }

    /// Tune the interrupt→SIGTERM→SIGKILL escalation timing.
    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(p) = &self.executable {
            return crate::executable::validate_native_override(p);
        }
        if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECUTABLE")
            && !p.is_empty()
        {
            return crate::executable::validate_native_override(&PathBuf::from(p));
        }
        resolve_claude_executable().ok_or_else(|| {
            HarnessError::NotInstalled(
                "claude (searched PATH, the login shell's PATH, ~/.claude/local, \
                 ~/.local/bin, /opt/homebrew/bin, /usr/local/bin, and \
                 fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE \
                 and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; set \
                 CLAUDE_CODE_EXECUTABLE to override)"
                    .into(),
            )
        })
    }

    fn build_command(&self, exe: &PathBuf, request: &RunRequest, tools: ToolSelection) -> Command {
        let mut cmd = Command::new(exe);
        crate::compose_child_path(&mut cmd, exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // Required by the CLI alongside `-p --output-format stream-json`.
            "--verbose",
            "--include-partial-messages",
            // Newer Claude models emit no readable thinking text unless a
            // summary is asked for (raw reasoning stays provider-private).
            "--thinking-display",
            "summarized",
        ]);
        // The 1M context window is selected via a model-id suffix
        // (`sonnet[1m]`), exactly how the CLI itself does it; fast mode and
        // always-on thinking are settings overrides.
        if let Some(model) = &request.model {
            let one_m = request
                .model_options
                .get("contextWindow")
                .and_then(Value::as_str)
                == Some("1m");
            cmd.arg("--model");
            cmd.arg(if one_m {
                format!("{model}[1m]")
            } else {
                model.clone()
            });
        }
        if let Some(effort) = to_effort(request.reasoning, request.model.as_deref()) {
            cmd.args(["--effort", effort]);
        }
        if let Some(contract) = request
            .model_options
            .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
            .and_then(|value| {
                serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone()).ok()
            })
            .filter(|contract| {
                contract.context.harness == HarnessId::ClaudeCode
                    && contract.delivery == luna_proto::RuntimeDelivery::ClaudeInstructionsOnly
            })
        {
            for (name, value) in &contract.environment {
                if matches!(
                    name.as_str(),
                    "LUNA_THREAD_ID"
                        | "LUNA_PROJECT_ID"
                        | "LUNA_ENVIRONMENT_ID"
                        | "LUNA_MACHINE_ID"
                ) {
                    cmd.env(name, value);
                }
            }
            let sections = contract
                .instructions
                .iter()
                .map(|section| {
                    format!(
                        "[LUNA {:?}: {} | {}]\n{}",
                        section.source, section.name, section.provenance, section.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            cmd.arg("--append-system-prompt");
            cmd.arg(sections);
        }
        if let Some(resume) = &request.resume {
            cmd.arg(format!("--resume={resume}"));
        }
        let mut settings = serde_json::Map::new();
        if option_is_on(&request.model_options, "fastMode") {
            settings.insert("fastMode".into(), Value::Bool(true));
        }
        if option_is_on(&request.model_options, "thinking") {
            settings.insert("alwaysThinkingEnabled".into(), Value::Bool(true));
        }
        if request.reasoning == Some(ReasoningLevel::Ultracode) {
            settings.insert("ultracode".into(), Value::Bool(true));
        }
        // Policy flags (incl. the stdio permission channel), the one merged
        // settings layer, cwd and provider home: shared with discovery (G1).
        let mut profile = ClaudeLaunchProfile::for_request(request, self.config_home.clone());
        profile.tools = tools;
        profile.apply(&mut cmd, &settings);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// Unscoped `initialize` probe used by model discovery: no workspace, no
    /// policy flags (unchanged), but never the user's hooks.
    async fn discover_initialize(&self) -> Result<Value, HarnessError> {
        let exe = self.resolve_executable()?;
        let home = self.config_home.clone();
        discovery::initialize_probe(
            &exe,
            |cmd| {
                cmd.arg("--settings");
                cmd.arg(Value::Object(profile::discovery_settings()).to_string());
                if let Some(home) = &home {
                    cmd.env("CLAUDE_CONFIG_DIR", home);
                }
            },
            self.kill_grace,
        )
        .await
    }

    /// The CLI's read-only initialization response contains its current model
    /// aliases and account-visible resolved models. No user message is sent.
    pub async fn live_models(&self) -> Result<Vec<Model>, HarnessError> {
        let response = self.discover_initialize().await?;
        let models = models_from_initialize(&response);
        if models.is_empty() {
            return Err(HarnessError::Protocol(
                "Claude initialize returned no models".into(),
            ));
        }
        Ok(models)
    }
}

/// `commands` out of an `initialize` control_response payload
/// (`response.response.commands`: name / description / argumentHint).
fn parse_initialize_commands(response: &Value) -> Vec<SlashCommand> {
    response
        .get("response")
        .and_then(|r| r.get("commands"))
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let name = c.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            Some(SlashCommand {
                name: name.to_owned(),
                description: c
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_hint: c
                    .get("argumentHint")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect()
}

#[async_trait]
impl Harness for ClaudeHarness {
    fn luna_runtime_delivery(&self) -> luna_proto::RuntimeDelivery {
        luna_proto::RuntimeDelivery::ClaudeInstructionsOnly
    }
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Claude Code"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    }
    fn installed(&self) -> bool {
        // The launch resolver, not bare discovery: a valid CLAUDE_CODE_EXECUTABLE
        // (or a test `executable`) must report installed, and an invalid one
        // must not — availability and launches share one resolution.
        self.resolve_executable().is_ok()
    }
    /// Done is the CLI's own terminal frame, for wake turns too.
    fn deterministic_turn_end(&self) -> bool {
        true
    }

    /// Keep the trait's lightweight bundled list for internal callers that
    /// are not presenting the model picker. The engine cache invokes
    /// `live_models` in the background and for explicit refreshes.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        Ok(static_models())
    }

    /// Legacy unscoped discovery: the default (Ask) profile, no workspace.
    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        self.commands_in(&CommandScope::default()).await
    }

    /// Slash commands from the CLI's `initialize` control-request handshake —
    /// the same channel the Claude Agent SDK's `query()` opens; no model turn.
    /// Spawned from the scope's launch profile (binary, home, cwd, policy), so
    /// the catalog is the one a run in that scope loads. Cached per scope.
    async fn commands_in(&self, scope: &CommandScope) -> Result<Vec<SlashCommand>, HarnessError> {
        let exe = self.resolve_executable()?;
        let profile = ClaudeLaunchProfile::new(
            scope.permission_mode,
            scope.cwd.as_deref().map(profile::canonical_dir),
            self.config_home.clone(),
        );
        let key = discovery::scope_key(&exe, &profile, scope);
        if let Some(commands) = self.discovery.get(key) {
            return Ok(commands);
        }
        let settings = profile::discovery_settings();
        let response =
            discovery::initialize_probe(&exe, |cmd| profile.apply(cmd, &settings), self.kill_grace)
                .await?;
        let commands = parse_initialize_commands(&response);
        self.discovery.put(key, commands.clone());
        Ok(commands)
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.run_with_mode(request, controls, false).await
    }

    async fn run_title(
        &self,
        mut request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        request.resume = None;
        request.worktree = None;
        request.attachments.clear();
        request.input_items.clear();
        request.model_options.clear();
        request.auto_approve = false;
        self.run_with_mode(request, controls, true).await
    }
}

impl ClaudeHarness {
    async fn run_with_mode(
        &self,
        mut request: RunRequest,
        controls: RunControls,
        title_only: bool,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if title_only {
            request
                .model_options
                .remove(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION);
        }
        if let Some(value) = request
            .model_options
            .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
        {
            let contract: luna_proto::LunaAgentRuntimeContract =
                serde_json::from_value(value.clone()).map_err(|error| {
                    HarnessError::Protocol(format!("invalid LUNA runtime contract: {error}"))
                })?;
            if contract.context.harness != HarnessId::ClaudeCode
                || contract.delivery != luna_proto::RuntimeDelivery::ClaudeInstructionsOnly
                || !contract.tools.is_empty()
            {
                return Err(HarnessError::Protocol(
                    "Claude LUNA runtime contract advertises unsupported tools".into(),
                ));
            }
        }
        request
            .model_options
            .remove(crate::LUNA_PROVIDER_AUTH_INSTANCE_OPTION);
        let exe = self.resolve_executable()?;
        let tools = if title_only {
            ToolSelection::Disabled
        } else {
            ToolSelection::ModeDefault
        };
        let mut cmd = self.build_command(&exe, &request, tools);
        if title_only {
            cmd.args([
                "--system-prompt",
                crate::TITLE_INSTRUCTIONS,
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--setting-sources",
                "",
            ]);
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdout".into()))?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "luna_harness::claude", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<StdinMsg>();
        tokio::spawn(stdin_writer(stdin, stdin_rx));

        // The initial prompt as the first stdin user line (streaming-input
        // mode). Ultrathink rides every user message — steers included.
        // Staged image attachments are inlined as base64 image content blocks
        // ahead of the text (verified against the real CLI); their path refs
        // also ride the prompt text, so a skipped/unreadable file degrades to
        // the old-app behavior (the agent opens the path with its Read tool).
        let images = load_image_blocks(&request.attachments).await;
        let first = wire::user_message_line_with_images(
            &apply_ultrathink(request.reasoning, &request.prompt),
            &images,
        );
        let permission_mode = request
            .permission_policy
            .as_ref()
            .map(|policy| policy.effective_mode)
            .unwrap_or(luna_proto::PermissionMode::Ask);
        let mut transport = transport::ControlChannel::new(
            profile::mode_args(permission_mode)
                .windows(2)
                .find(|pair| pair[0] == "--permission-mode")
                .map(|pair| pair[1]),
        );
        // One uuid per send: the transport mints it (lifecycle receipts) and
        // the CC-02 turn ledger correlates results against the same uuid.
        let mut norm = Normalizer::new();
        let (first, initial_uuid) =
            transport.stamp_user_line(first, transport::InputKind::Initial, None);
        norm.native
            .ledger
            .register_native(turns::SendKind::Initial, None, initial_uuid);
        let _ = stdin_tx.send(StdinMsg::line(first));

        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            norm,
            title_only,
            child,
            stdout_lines: BufReader::new(stdout).lines(),
            stdin_tx,
            event_tx,
            controls,
            reasoning: request.reasoning,
            permission_mode,
            transport,
            permission_policy: request.permission_policy.clone().unwrap_or_default(),
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            stderr_tail,
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

enum StdinMsg {
    Line {
        line: String,
        written: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    /// Close stdin (end of steering input): the CLI finishes the current turn
    /// and exits, which ends the run stream at stdout EOF.
    Close,
}

impl StdinMsg {
    fn line(line: String) -> Self {
        Self::Line {
            line,
            written: None,
        }
    }
}

/// Anthropic's API caps inline images at 5MB of raw bytes; larger files stay
/// path refs only.
const MAX_INLINE_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Media type for an inline image block — extension first, magic bytes as the
/// fallback (pasted screenshots may carry odd names). Only the API-supported
/// inline types map; anything else (svg/bmp/tiff/…) returns `None`.
fn image_media_type(path: &std::path::Path, bytes: &[u8]) -> Option<&'static str> {
    let by_ext = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    };
    by_ext.or(match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    })
}

/// Load `RunRequest::attachments` into inline image blocks, best-effort: an
/// unreadable, oversized, or unsupported file is skipped — its path ref still
/// rides the prompt text — never fatal to the run.
async fn load_image_blocks(paths: &[String]) -> Vec<wire::ImageBlock> {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    for path in paths {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(target: "luna_harness::claude", %path, error = %err, "attachment unreadable; path ref only");
                continue;
            }
        };
        if bytes.len() as u64 > MAX_INLINE_IMAGE_BYTES {
            tracing::debug!(target: "luna_harness::claude", %path, "attachment over inline cap; path ref only");
            continue;
        }
        let Some(media_type) = image_media_type(std::path::Path::new(path), &bytes) else {
            tracing::debug!(target: "luna_harness::claude", %path, "attachment not an inline-supported image; path ref only");
            continue;
        };
        blocks.push(wire::ImageBlock {
            media_type: media_type.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    blocks
}

/// Owns the child's stdin; a write failure (EPIPE after the child died) is
/// tolerated and logged.
async fn stdin_writer(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<StdinMsg>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            StdinMsg::Line { line, written } => {
                let write = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                match write.await {
                    Ok(()) => {
                        if let Some(written) = written {
                            let _ = written.send(Ok(()));
                        }
                    }
                    Err(e) => {
                        if let Some(written) = written {
                            let _ = written.send(Err(e.to_string()));
                        }
                        tracing::debug!(target: "luna_harness::claude", "stdin write failed (tolerated): {e}");
                        return;
                    }
                }
            }
            StdinMsg::Close => {
                let _ = stdin.shutdown().await;
                return;
            }
        }
    }
}

struct Session {
    norm: Normalizer,
    title_only: bool,
    child: Child,
    stdout_lines: tokio::io::Lines<BufReader<crate::process::ChildStdout>>,
    stdin_tx: mpsc::UnboundedSender<StdinMsg>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    reasoning: Option<ReasoningLevel>,
    permission_mode: luna_proto::PermissionMode,
    /// Control-channel state: request ids, receipts, capabilities.
    transport: transport::ControlChannel,
    permission_policy: luna_proto::PermissionPolicy,
    interrupt_grace: Duration,
    kill_grace: Duration,
    /// Rolling stderr tail for the crash message on an unexpected exit.
    stderr_tail: crate::StderrTail,
}

/// The per-run event loop: one task multiplexing stdout frames, the steering
/// mailbox, the interrupt token, and consumer liveness.
async fn run_session(session: Session) {
    let Session {
        mut norm,
        title_only,
        mut child,
        mut stdout_lines,
        stdin_tx,
        event_tx,
        controls,
        reasoning,
        permission_mode,
        mut transport,
        permission_policy,
        interrupt_grace,
        kill_grace,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        ..
    } = controls;
    // CC-03: the single owner of CLI→host control requests.
    let requests = controls::Controls::new(
        title_only,
        permission_mode,
        permission_policy,
        Arc::new(request_input),
        stdin_tx.clone(),
    );
    // Stop, or LUNA failing closed on a weaker native permission mode.
    let stop = interrupt.child_token();

    // Claude's root user echo carries no correlation id. Explicit steering is
    // single-flight and a successful stdin write remains unconfirmed; text
    // equality alone must never settle a durable queue row.
    let mut explicit_steer_unconfirmed = false;
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut any_done = false;
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        let receipt_deadline = transport.next_deadline();
        tokio::select! {
            line = stdout_lines.next_line() => match line {
                Ok(Some(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some(id) = controls::cancelled_request_id(line) {
                        requests.cancel(&id);
                        continue;
                    }
                    if let transport::Intercept::Consumed(effects) = transport.intercept(line) {
                        for user_message_id in effects.incorporated {
                            // Exact native receipt: this steer's uuid drained into a turn.
                            explicit_steer_unconfirmed = false;
                            let (prev, next) = norm.rotate_for_steer();
                            let ev = AgentEvent::Steered {
                                user_message_id: Some(user_message_id),
                                assistant_message_id: Some(prev),
                                next_assistant_message_id: Some(next),
                            };
                            if event_tx.send(Ok(ev)).await.is_err() {
                                break 'main;
                            }
                        }
                        // CC-02: the same receipts drive the turn ledger
                        // (deferred follow-up boundaries, withheld foreign Done).
                        for (uuid, state) in effects.lifecycle {
                            for ev in norm.native_input_lifecycle(&uuid, state.as_str()) {
                                if event_tx.send(Ok(ev)).await.is_err() {
                                    break 'main;
                                }
                            }
                        }
                        continue;
                    }
                    let frame = match wire::parse_frame(line) {
                        Ok(frame) => frame,
                        Err(e) => {
                            tracing::debug!(target: "luna_harness::claude", "unparseable frame (retained as evidence): {e}");
                            for ev in norm.native.invalid_line(line, &e) {
                                if event_tx.send(Ok(ev)).await.is_err() {
                                    break 'main;
                                }
                            }
                            continue;
                        }
                    };
                    if let Frame::ControlRequest(req) = frame {
                        requests.on_request(&req.request_id, &req.request);
                        continue;
                    }
                    if let Frame::System(system) = &frame
                        && let Some(verdict) = requests.observe_system(
                            &system.subtype,
                            &system.session_id,
                            &system.cwd,
                            system.permission_mode.as_deref(),
                        )
                    {
                        let error = AgentEvent::Error { message: verdict.message };
                        if event_tx.send(Ok(error)).await.is_err() {
                            break 'main;
                        }
                        if verdict.fail_closed {
                            stop.cancel();
                        }
                    }
                    for ev in norm.normalize_native(frame, interrupted) {
                        let is_done = matches!(ev, AgentEvent::Done { .. });
                        if is_done {
                            // Stream-json result frames are the authoritative
                            // per-turn terminal boundary. A native stdin ACK
                            // that never produced correlated incorporation is
                            // no longer allowed to block the next user turn.
                            explicit_steer_unconfirmed = false;
                        }
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main; // consumer gone — reap below
                        }
                        if is_done {
                            any_done = true;
                            if interrupted {
                                done_after_interrupt = true;
                                break 'main;
                            }
                        }
                    }
                }
                Ok(None) => break 'main, // stdout EOF: the CLI exited
                Err(e) => {
                    let _ = event_tx.send(Err(HarnessError::Io(e))).await;
                    break 'main;
                }
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    let prompt = apply_ultrathink(reasoning, &msg.prompt);
                    let explicit = msg.delivery == crate::SteerDelivery::RequireActiveTurn;
                    let (line, uuid) = transport.stamp_user_line(
                        wire::user_message_line(&prompt),
                        if explicit { transport::InputKind::Explicit } else { transport::InputKind::Ordinary },
                        msg.message_id.clone(),
                    );
                    if explicit {
                        if explicit_steer_unconfirmed {
                            if let Some(tx) = msg.acceptance {
                                let _ = tx.send(crate::SteerAcceptance::Rejected(
                                    "another Claude steer is accepted but incorporation is unconfirmed"
                                        .into(),
                                ));
                            }
                            continue 'main;
                        }
                        norm.native.ledger.register_native(
                            turns::SendKind::Steer,
                            msg.message_id.clone(),
                            uuid.clone(),
                        );
                        let (written_tx, written_rx) = tokio::sync::oneshot::channel();
                        if stdin_tx.send(StdinMsg::Line {
                            line,
                            written: Some(written_tx),
                        }).is_err() {
                            if let Some(tx) = msg.acceptance {
                                let _ = tx.send(crate::SteerAcceptance::Rejected(
                                    "Claude input channel closed before dispatch".into(),
                                ));
                            }
                            continue 'main;
                        }
                        let written = tokio::select! {
                            biased;
                            _ = interrupt.cancelled() => None,
                            _ = event_tx.closed() => None,
                            written = written_rx => Some(written),
                        };
                        let Some(written) = written else {
                            if let Some(tx) = msg.acceptance {
                                let _ = tx.send(crate::SteerAcceptance::Unknown(
                                    "Claude session stopped before confirming the write".into(),
                                ));
                            }
                            interrupted = interrupt.is_cancelled();
                            break 'main;
                        };
                        match written {
                            Ok(Ok(())) => {
                                explicit_steer_unconfirmed = true;
                                transport.explicit_written(&uuid, msg.acceptance);
                            }
                            Ok(Err(error)) => {
                                if let Some(tx) = msg.acceptance {
                                    let _ = tx.send(crate::SteerAcceptance::Unknown(error));
                                }
                            }
                            Err(_) => {
                                if let Some(tx) = msg.acceptance {
                                    let _ = tx.send(crate::SteerAcceptance::Unknown(
                                        "Claude stdin writer ended before confirming the write"
                                            .into(),
                                    ));
                                }
                            }
                        }
                    } else {
                        norm.native.ledger.register_native(
                            turns::SendKind::FollowUp,
                            msg.message_id.clone(),
                            uuid,
                        );
                        let accepted = stdin_tx.send(StdinMsg::line(line)).is_ok();
                        if !accepted {
                            continue 'main;
                        }
                        // CC-02: an echo-capable CLI may queue this send
                        // behind a turn it started itself; its `Steered`
                        // waits for the native start of THIS send.
                        if norm.native.ledger.defers_follow_up_boundary() {
                            continue 'main;
                        }
                        // Ordinary warm routing retains its historical local
                        // boundary/follow-up behavior.
                        let (prev, next) = norm.rotate_for_steer();
                        let ev = AgentEvent::Steered {
                            user_message_id: None,
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        };
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main;
                        }
                    }
                }
                None => {
                    // Mailbox closed: end the input so the run can finish
                    // after the current turn.
                    steering_open = false;
                    let _ = stdin_tx.send(StdinMsg::Close);
                }
            },

            () = sleep_until(receipt_deadline), if receipt_deadline.is_some() => {
                transport.expire_due(std::time::Instant::now());
            },

            _ = stop.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                // Pending prompts are withdrawn, never answered after Stop.
                requests.cancel_all();
                let _ = stdin_tx.send(StdinMsg::line(transport.interrupt_line()));
                // Escalate if the CLI doesn't wind down within the grace
                // periods: SIGTERM (kills bash trees, runs SessionEnd hooks),
                // then SIGKILL. Aborted once the child is reaped.
                if let Some(pid) = crate::process::signal_target(&child) {
                    escalation = Some(tokio::spawn(async move {
                        tokio::time::sleep(interrupt_grace).await;
                        send_signal(&pid, Signal::Term);
                        tokio::time::sleep(kill_grace).await;
                        send_signal(&pid, Signal::Kill);
                    }));
                }
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    // Terminal bookkeeping: never end the stream without a Done unless the
    // consumer already hung up.
    if !event_tx.is_closed() {
        if interrupted && !done_after_interrupt {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: norm.session_id.clone(),
                }))
                .await;
        } else if !interrupted && !any_done {
            let status = child.try_wait().ok().flatten();
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message("claude", status, &stderr_tail)),
                    session_id: norm.session_id.clone(),
                }))
                .await;
        }
    }

    requests.cancel_all();
    transport.finish();
    if done_after_interrupt {
        // Stop = interrupt, then end input and let the CLI exit on its own
        // before the SIGTERM → SIGKILL ladder.
        let _ = stdin_tx.send(StdinMsg::Close);
        transport::drain_exit(&mut child, kill_grace).await;
    }
    shutdown_child(&mut child, kill_grace).await;
    if let Some(handle) = escalation {
        handle.abort();
    }
}

async fn sleep_until(deadline: Option<std::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline.into()).await;
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_modes_map_to_exact_cli_arguments() {
        use luna_proto::PermissionMode::{Ask, Auto, Full};
        assert_eq!(
            profile::mode_args(Ask),
            ["--restricted", "--permission-mode", "acceptEdits"]
        );
        assert_eq!(
            profile::mode_args(Auto),
            ["--restricted", "--permission-mode", "auto"]
        );
        assert_eq!(
            profile::mode_args(Full),
            [
                "--permission-mode",
                "bypassPermissions",
                "--dangerously-skip-permissions"
            ]
        );
    }
}
