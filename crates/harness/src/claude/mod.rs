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

mod accounting;
pub mod background_sessions;
pub mod catalog;
mod controls;
mod coordination;
mod delivery;
mod discovery;
mod events;
pub mod extensions;
mod findings;
mod goal_control;
mod goals;
mod history;
mod hooks;
mod input;
mod live_session;
mod luna_tools;
mod mcp;
mod native_stop;
mod native_tasks;
mod normalize;
mod ongoing;
pub mod peer;
pub mod profile;
mod runtime_controls;
mod saved_output;
mod schedules;
mod tool_results;
mod transport;
mod turns;
mod wire;

pub use input::validate_inputs;
/// Shared with the OpenCode MCP readback (provider errors echo URLs/paths).
pub(crate) use mcp::redact_provider_text;

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
use catalog::{models_from_initialize, static_models};
use normalize::Normalizer;
use profile::{ClaudeLaunchProfile, ToolSelection};
/// Model options a native live switch may change (CC-06); the engine requires
/// every other option of a warm runtime to be unchanged.
pub use runtime_controls::SWITCHABLE_OPTIONS as LIVE_SWITCH_OPTIONS;
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
    /// CC-06: live model/effort switches staged by the engine for a warm run,
    /// keyed by the follow-up steer's message id.
    staged: runtime_controls::SharedStaged,
    /// MCP reports from each run's own `system:init` (CC-11), in memory only.
    mcp_sessions: Arc<mcp::SessionMcpRegistry>,
    /// Live runs addressable by `stop_native_child` (CC-08).
    live_children: native_stop::LiveChildren,
    /// Managed-settings directory override (tests/fixtures); `None` reads the
    /// platform location (`extensions.rs`).
    managed_dir: Option<PathBuf>,
    /// Verified extension views per (cwd, mode), reused until the
    /// extension fingerprint changes.
    extension_views: extensions::ViewCache,
}

impl Default for ClaudeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            config_home: None,
            discovery: discovery::DiscoveryCache::default(),
            staged: Default::default(),
            mcp_sessions: Arc::default(),
            live_children: native_stop::LiveChildren::default(),
            managed_dir: None,
            extension_views: Default::default(),
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

    /// Where Claude persists this harness's session records (CC-14): the
    /// pinned provider home (`with_config_dir`), else the user's.
    fn goal_config_dir(&self) -> Option<PathBuf> {
        self.config_home.clone().or_else(goals::default_config_dir)
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
        coordination::scrub_inherited_session_env(&mut cmd);
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
            // CC-06/CC-09: re-emit each stdin user line with its `uuid` and
            // provenance once the CLI records it — the native delivery receipt
            // (folded into the transport's single uuid mechanism) and the
            // attribution of native peer arrivals (see `peer`).
            "--replay-user-messages",
        ]);
        // CC-15: LUNA renders Claude's typed review findings, so `/code-review`
        // reports through `ReportFindings` (Claude's host opt-in).
        cmd.env("CLAUDE_CODE_REPORT_FINDINGS", "1");
        // The 1M context window is selected via a model-id suffix
        // (`sonnet[1m]`), exactly how the CLI itself does it; fast mode and
        // always-on thinking are settings overrides.
        let model_flag =
            runtime_controls::model_flag(request.model.as_deref(), &request.model_options);
        if let Some(model) = &model_flag {
            cmd.args(["--model", model]);
        }
        // CC-06: the discovered effort ladder decides when one is known.
        if let Some(effort) =
            runtime_controls::effort_flag(exe, request.reasoning, model_flag.as_deref())
        {
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
                    && matches!(
                        contract.delivery,
                        luna_proto::RuntimeDelivery::ClaudeInstructionsOnly
                            | luna_proto::RuntimeDelivery::ClaudeMcpTools
                    )
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
        // CC-10: the SDK's enableFileCheckpointing, so `rewind_files` can
        // restore Edit/Write/NotebookEdit changes later.
        cmd.env(history::FILE_CHECKPOINT_ENV, "1");
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
                coordination::scrub_inherited_session_env(cmd);
                cmd.arg("--settings");
                cmd.arg(Value::Object(profile::discovery_settings()).to_string());
                if let Some(home) = &home {
                    cmd.env("CLAUDE_CONFIG_DIR", home);
                }
            },
            self.kill_grace,
        )
        .await
        .inspect(|response| runtime_controls::remember_catalog(&exe, response))
    }

    /// Provider-owned subscription read. Sends only initialization and
    /// `get_usage`; the CLI owns credential storage and renewal. No model turn,
    /// saved conversation, user hook, project tools or MCP discovery is needed.
    pub async fn subscription_usage(&self) -> Result<Value, HarnessError> {
        let exe = self.resolve_executable()?;
        let home = self.config_home.clone();
        discovery::usage_probe(
            &exe,
            |cmd| {
                coordination::scrub_inherited_session_env(cmd);
                cmd.arg("--settings")
                    .arg(Value::Object(profile::discovery_settings()).to_string());
                cmd.env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
                    .env("CLAUDE_CODE_AUTO_CONNECT_IDE", "0")
                    .env("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL", "1");
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
                kind: Default::default(),
            })
        })
        .collect()
}

#[async_trait]
impl Harness for ClaudeHarness {
    /// LUNA tools ride the shared per-process `luna` MCP endpoint
    /// (`luna_tools`), on fresh and resumed sessions alike.
    fn luna_runtime_delivery(&self) -> luna_proto::RuntimeDelivery {
        luna_proto::RuntimeDelivery::ClaudeMcpTools
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

    fn can_reconfigure_live(
        &self,
        from: &crate::LiveRuntimeSelection,
        to: &crate::LiveRuntimeSelection,
    ) -> bool {
        self.resolve_executable()
            .is_ok_and(|exe| runtime_controls::plan_switch(&exe, from, to).is_some())
    }

    fn stage_live_reconfiguration(
        &self,
        message_id: &str,
        from: &crate::LiveRuntimeSelection,
        to: &crate::LiveRuntimeSelection,
    ) -> bool {
        self.resolve_executable()
            .is_ok_and(|exe| runtime_controls::stage(&self.staged, &exe, message_id, from, to))
    }

    /// Keep the trait's lightweight bundled list for internal callers that
    /// are not presenting the model picker. The engine cache invokes
    /// `live_models` in the background and for explicit refreshes.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        Ok(static_models())
    }

    /// The engine cache's refresh (startup, every two hours, Refresh models):
    /// the CLI's own account-visible list from `initialize`, so a model
    /// released after this build appears without a LUNA update. A failed
    /// probe reaches the cache, which keeps its last good snapshot.
    async fn models_live(&self) -> Result<Vec<Model>, HarnessError> {
        self.live_models().await
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
        let response = discovery::initialize_probe(
            &exe,
            |cmd| {
                coordination::scrub_inherited_session_env(cmd);
                profile.apply(cmd, &settings);
            },
            self.kill_grace,
        )
        .await?;
        let mut commands = parse_initialize_commands(&response);
        // The catalog does not say which entries are skills. Tag the ones a
        // skill folder backs so the composer offers them under `$`; the
        // invocation Claude receives stays `/name`.
        let skills = extensions::skill_folder_names(
            profile.effective_config_home().as_deref(),
            profile.cwd.as_deref(),
        );
        for command in &mut commands {
            if skills.contains(&command.name) {
                command.kind = luna_proto::SlashCommandKind::Skill;
            }
        }
        self.discovery.put(key, commands.clone());
        Ok(commands)
    }

    async fn native_extensions(
        &self,
        cwd: Option<&std::path::Path>,
        mode: luna_proto::PermissionMode,
        refresh: bool,
    ) -> Result<luna_proto::NativeExtensionsView, HarnessError> {
        self.native_extensions_view(cwd, mode, refresh).await
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.run_with_mode(request, controls, false).await
    }

    async fn mcp_control(
        &self,
        request: crate::mcp_control::McpControlRequest,
    ) -> Result<crate::mcp_control::McpControlReply, HarnessError> {
        mcp::control(self.resolve_executable()?, self.kill_grace, request).await
    }

    fn session_mcp_observation(
        &self,
        provider_session_id: &str,
    ) -> Option<crate::mcp_control::SessionMcpObservation> {
        self.mcp_sessions.get(provider_session_id)
    }

    /// CC-10: Claude forks follow the Agent SDK `forkSession` contract.
    fn fork_capability(&self) -> luna_proto::ProviderForkCapability {
        luna_proto::ProviderForkCapability::Checkpoint
    }
    async fn fork_session(
        &self,
        request: luna_proto::ProviderForkRequest,
    ) -> Result<luna_proto::ProviderForkResult, HarnessError> {
        self.fork_session_staged(request, &|_| Ok(())).await
    }
    async fn fork_session_staged(
        &self,
        request: luna_proto::ProviderForkRequest,
        progress: &crate::ProviderForkProgressReporter,
    ) -> Result<luna_proto::ProviderForkResult, HarnessError> {
        history::fork_session(&self.resolve_executable()?, request, progress).await
    }
    async fn native_history_turns(
        &self,
        scope: crate::NativeHistoryScope,
    ) -> Result<crate::NativeHistoryTurns, HarnessError> {
        history::native_turns(&scope)
    }
    async fn rewind_native_conversation(
        &self,
        request: crate::NativeRewindRequest,
        progress: &crate::NativeRewindProgressReporter,
    ) -> Result<crate::NativeRewindOutcome, HarnessError> {
        history::rewind_conversation(&self.resolve_executable()?, request, progress).await
    }
    async fn rewind_native_files(
        &self,
        request: crate::NativeFileRewindRequest,
    ) -> Result<crate::NativeFileRewindResult, HarnessError> {
        history::rewind_files(&self.resolve_executable()?, request).await
    }

    fn supports_native_child_stop(&self) -> bool {
        true
    }

    async fn stop_native_child(
        &self,
        native_session_id: &str,
        spawn_tool_use_id: &str,
    ) -> Result<crate::NativeTaskStop, HarnessError> {
        Ok(self
            .live_children
            .stop(native_session_id, spawn_tool_use_id)
            .await)
    }

    /// CC-14: Claude's own goal record and `/goal clear` (see `goal_control`).
    async fn native_goal_control(
        &self,
        request: crate::NativeGoalControl,
    ) -> Result<crate::NativeGoalReadback, HarnessError> {
        goal_control::run(self, request).await
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
        let mut luna = None;
        if let Some(value) = request
            .model_options
            .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
        {
            let contract: luna_proto::LunaAgentRuntimeContract =
                serde_json::from_value(value.clone()).map_err(|error| {
                    HarnessError::Protocol(format!("invalid LUNA runtime contract: {error}"))
                })?;
            let tools_deliverable = contract.delivery
                == luna_proto::RuntimeDelivery::ClaudeMcpTools
                && controls.invoke_luna_tool.is_some();
            if contract.context.harness != HarnessId::ClaudeCode
                || !matches!(
                    contract.delivery,
                    luna_proto::RuntimeDelivery::ClaudeInstructionsOnly
                        | luna_proto::RuntimeDelivery::ClaudeMcpTools
                )
                || (!contract.tools.is_empty() && !tools_deliverable)
            {
                return Err(HarnessError::Protocol(
                    "Claude LUNA runtime contract advertises unsupported tools".into(),
                ));
            }
            // Bound before the spawn so the CLI connects at MCP startup.
            if let (false, Some(executor)) = (contract.tools.is_empty(), &controls.invoke_luna_tool)
            {
                luna = Some(
                    luna_tools::ClaudeLunaBridge::bind(
                        contract,
                        Arc::clone(executor),
                        controls.interrupt.clone(),
                    )
                    .await?,
                );
            }
        }
        request
            .model_options
            .remove(crate::LUNA_PROVIDER_AUTH_INSTANCE_OPTION);
        let exe = self.resolve_executable()?;
        // One typed builder for every turn. It validates the exact bytes it
        // sends, so an invalid or changed file rejects here, before a process
        // or stdin line exists, and the caller keeps the draft envelope.
        let first = input::user_line(
            &request.prompt,
            request.reasoning,
            &request.attachments,
            &request.input_items,
        )
        .await?;
        tracing::info!(target: "luna_harness::claude", input = %first.manifest.summary(), "first user line");
        let no_file_edits = request
            .model_options
            .remove(crate::LUNA_NO_FILE_EDITS_OPTION)
            .is_some_and(|value| value == Value::Bool(true));
        let tools = if title_only {
            ToolSelection::Disabled
        } else if no_file_edits {
            // A run that must never change files (a Board ticket discussion):
            // the editing tools are not offered at all.
            ToolSelection::NoEdits
        } else if request.sandbox == luna_proto::SandboxLevel::ReadOnly
            && request
                .permission_policy
                .as_ref()
                .is_some_and(|policy| policy.effective_mode != luna_proto::PermissionMode::Full)
        {
            // Engine-resolved read-only (a review-constrained reviewer):
            // no edit, write or command tools at all.
            ToolSelection::ReadOnly
        } else {
            ToolSelection::ModeDefault
        };
        // CC-13: never become a second writer of a session another live
        // Claude process owns.
        if !title_only && let Some(resume) = request.resume.as_deref() {
            background_sessions::refuse_concurrent_writer(
                &exe,
                resume,
                self.config_home.as_deref(),
            )
            .await?;
        }
        let mut cmd = self.build_command(&exe, &request, tools);
        let (luna_endpoint, luna) = match luna {
            Some((endpoint, attachment, bridge)) => {
                cmd.args(&attachment.args);
                cmd.env(attachment.env.0, &attachment.env.1);
                (Some(endpoint), Some(bridge))
            }
            None => (None, None),
        };
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
        } else {
            if crate::mcp_isolation::qa_enabled() {
                if luna_endpoint.is_some() {
                    // Keep only the per-process LUNA tools attached above.
                    crate::mcp_isolation::claude_strict_flag(&mut cmd);
                } else {
                    crate::mcp_isolation::claude_strict_empty(&mut cmd);
                }
            }
            cmd.args(native_tasks::RUN_ARGS);
            // SessionStart/Setup stream by default; every other hook event
            // needs this opt-in (`hooks.rs`).
            cmd.arg("--include-hook-events");
        }
        // CC-12: a typed skill must be in this launch's effective catalog; the
        // CC-04 line builder writes its invocation.
        self.typed_skill_for_launch(&exe, &cmd, &request).await?;
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
        // CC-14: a goal start is Claude's own `/goal <condition>` line; it
        // still takes the single native uuid below.
        let saved_output = saved_output::SavedOutputReader::new(
            self.goal_config_dir(),
            &request.cwd,
            request.resume.as_deref(),
        );
        let mut ongoing = ongoing::Ongoing::new(!title_only, self.goal_config_dir(), &request.cwd);
        let mut first_line = first.line;
        if let Some(goal) = request.goal.as_ref().filter(|_| !title_only) {
            ongoing.goal_requested(&goal.objective);
            first_line = wire::user_message_line(&goals::set_line(&goal.objective));
        }
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
            transport.stamp_user_line(first_line, transport::InputKind::Initial, None);
        // CC-10 checkpoints key on the same client uuid.
        let mut identities = history::TurnIdentities::default();
        if !title_only {
            identities.record(initial_uuid.clone());
        }
        norm.native
            .ledger
            .register_native(turns::SendKind::Initial, None, initial_uuid);
        let _ = stdin_tx.send(StdinMsg::line(first));
        // CC-06: read back what the CLI actually applied when LUNA asked for
        // a model or effort (a run that requested nothing has nothing to check).
        let launch_readback = (!title_only
            && (request.model.is_some() || request.reasoning.is_some()))
        .then(|| {
            let model =
                runtime_controls::model_flag(request.model.as_deref(), &request.model_options);
            let effort = runtime_controls::effort_flag(&exe, request.reasoning, model.as_deref())
                .map(str::to_owned);
            (model, effort)
        });

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
            cc06: Cc06Session {
                launch_readback,
                staged: self.staged.clone(),
            },
            mcp_sessions: (!title_only).then(|| self.mcp_sessions.clone()),
            identities,
            live_children: self.live_children.clone(),
            ongoing,
            saved_output,
            luna_endpoint,
            luna,
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
    /// This process's `luna` MCP endpoint; dropping it retires the token.
    luna_endpoint: Option<crate::codex::luna_mcp::LunaMcpEndpoint>,
    luna: Option<luna_tools::ClaudeLunaBridge>,
    /// CC-14/15 observers: native goals, findings and schedules.
    ongoing: ongoing::Ongoing,
    saved_output: saved_output::SavedOutputReader,
    live_children: native_stop::LiveChildren,
    /// CC-10 client uuids and per-turn provider checkpoints.
    identities: history::TurnIdentities,
    mcp_sessions: Option<Arc<mcp::SessionMcpRegistry>>,
    cc06: Cc06Session,
}

/// CC-06 launch inputs for delivery receipts, readback and live switches.
struct Cc06Session {
    /// Requested `(--model, --effort)` to verify with `get_settings`.
    launch_readback: Option<(Option<String>, Option<String>)>,
    staged: runtime_controls::SharedStaged,
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
        mut reasoning,
        permission_mode,
        mut transport,
        permission_policy,
        interrupt_grace,
        kill_grace,
        stderr_tail,
        cc06,
        mcp_sessions,
        mut identities,
        live_children,
        mut ongoing,
        mut saved_output,
        mut luna_endpoint,
        mut luna,
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

    let mut live = live_session::LiveSession::start(cc06, title_only, &stdin_tx);
    let mut released: std::collections::VecDeque<crate::SteerMessage> =
        std::collections::VecDeque::new();
    // Native children of this run become addressable by `stop_task` once
    // its session id is known (CC-08).
    let stoppable = norm.stoppable();
    let mut live_registration: Option<native_stop::Registration> = None;
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
        let switch_deadline = live.switch_deadline();
        let luna_deadline = luna
            .as_ref()
            .and_then(luna_tools::ClaudeLunaBridge::next_deadline);
        tokio::select! {
            line = stdout_lines.next_line() => match line {
                Ok(Some(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if native_stop::resolve_control_response(&stoppable, line) {
                        continue;
                    }
                    if let Some(id) = controls::cancelled_request_id(line) {
                        requests.cancel(&id);
                        continue;
                    }
                    // CC-06 side channel: replay receipts, its own control
                    // replies (live switch, launch readback) and accounting.
                    let prefix = match live.on_line(line, &stdin_tx, &mut reasoning) {
                        live_session::LineOutcome::Normalize { prefix } => prefix,
                        live_session::LineOutcome::Consumed(events) => {
                            for ev in events {
                                if event_tx.send(Ok(ev)).await.is_err() {
                                    break 'main;
                                }
                            }
                            continue;
                        }
                        live_session::LineOutcome::Release { events, steer } => {
                            released.push_back(*steer);
                            for ev in events {
                                if event_tx.send(Ok(ev)).await.is_err() {
                                    break 'main;
                                }
                            }
                            continue;
                        }
                        live_session::LineOutcome::SwitchFailed(events) => {
                            steering_open = false;
                            for ev in events {
                                if event_tx.send(Ok(ev)).await.is_err() {
                                    break 'main;
                                }
                            }
                            continue;
                        }
                        live_session::LineOutcome::Replay(uuid) => {
                            let own = transport.is_own_input(&uuid);
                            let effects = transport.on_replay(&uuid);
                            for ev in receipt_events(&mut norm, effects, &mut explicit_steer_unconfirmed) {
                                if event_tx.send(Ok(ev)).await.is_err() {
                                    break 'main;
                                }
                            }
                            if own {
                                continue;
                            }
                            // A replay LUNA never sent (a native peer arrival,
                            // CLI-originated input): CC-09 attributes it below.
                            Vec::new()
                        }
                    };
                    if let transport::Intercept::Consumed(effects) = transport.intercept(line) {
                        for ev in receipt_events(&mut norm, effects, &mut explicit_steer_unconfirmed) {
                            if event_tx.send(Ok(ev)).await.is_err() {
                                break 'main;
                            }
                        }
                        continue;
                    }
                    for ev in prefix {
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main;
                        }
                    }
                    // CC-14/15: native goals, findings and schedules read the
                    // raw frame first. They only add events.
                    let (mut extra, work) = ongoing.observe(line);
                    for item in work {
                        extra.extend(ongoing.resolve(item).await);
                    }
                    for ev in extra {
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main;
                        }
                    }
                    identities.observe(line);
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
                    if let (Frame::System(system), Some(registry)) = (&frame, &mcp_sessions) {
                        registry.observe(system);
                    }
                    if let Frame::ControlRequest(req) = frame {
                        if luna.as_ref().is_some_and(|luna| luna.preapproved(&req)) {
                            // CC-07: a `luna` tool the contract attached is
                            // LUNA's own call; its fence/approval runs in the
                            // engine dispatcher, never a second card here.
                            let input = req.request.get("input").cloned().unwrap_or(Value::Null);
                            let line = wire::control_response_line(
                                &req.request_id,
                                serde_json::json!({ "behavior": "allow", "updatedInput": input }),
                            );
                            let _ = stdin_tx.send(StdinMsg::line(line));
                        } else {
                            requests.on_request(&req.request_id, &req.request);
                        }
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
                    saved_output.observe(&frame);
                    let saved_admission = saved_output.admission(&frame);
                    let luna_turn = luna.as_mut().and_then(|luna| luna.observe(&frame));
                    for mut ev in norm
                        .normalize_native(frame, interrupted)
                        .into_iter()
                        .chain(luna_turn)
                    {
                        saved_output.enrich(&mut ev, saved_admission, &stop).await;
                        if let Some(checkpoint) = identities.checkpoint_before(&ev)
                            && event_tx.send(Ok(checkpoint)).await.is_err()
                        {
                            break 'main;
                        }
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
                    if live_registration.is_none()
                        && !title_only
                        && let Some(session_id) = norm.session_id.as_deref()
                    {
                        live_registration = Some(live_children.register(
                            session_id,
                            stdin_tx.clone(),
                            stoppable.clone(),
                        ));
                    }
                }
                Ok(None) => break 'main, // stdout EOF: the CLI exited
                Err(e) => {
                    let _ = event_tx.send(Err(HarnessError::Io(e))).await;
                    break 'main;
                }
            },

            // A steer parked behind a CC-06 live switch is released into
            // this same write path once the switch is applied and read back.
            steer = async {
                match released.pop_front() {
                    Some(steer) => Some(steer),
                    None => steering.recv().await,
                }
            }, if steering_open && !interrupted && !live.switch_pending() => match steer {
                Some(msg) => {
                    let msg = match live.route_steer(msg, &stdin_tx, &mut reasoning) {
                        live_session::SteerRoute::Pending => continue 'main,
                        live_session::SteerRoute::Write(msg) => *msg,
                    };
                    let line = match input::user_line(
                        &msg.prompt,
                        reasoning,
                        &msg.attachments,
                        &msg.input_items,
                    )
                    .await
                    {
                        Ok(input) => {
                            tracing::info!(target: "luna_harness::claude", input = %input.manifest.summary(), "steer user line");
                            input.line
                        }
                        Err(error) => {
                            // Nothing was written: explicit steering reports a
                            // recoverable rejection. Warm routes never carry
                            // typed inputs to Claude (engine `can_route`).
                            tracing::error!(target: "luna_harness::claude", %error, "steer input rejected before write");
                            if let Some(tx) = msg.acceptance {
                                let _ = tx.send(crate::SteerAcceptance::Rejected(error.to_string()));
                            }
                            continue 'main;
                        }
                    };
                    // CC-14: a warm goal start is Claude's own `/goal` line.
                    let mut line = line;
                    if let Some(goal) = &msg.goal {
                        ongoing.goal_requested(&goal.objective);
                        line = wire::user_message_line(&goals::set_line(&goal.objective));
                    }
                    let explicit = msg.delivery == crate::SteerDelivery::RequireActiveTurn;
                    let (line, uuid) = transport.stamp_user_line(
                        line,
                        if explicit { transport::InputKind::Explicit } else { transport::InputKind::Ordinary },
                        msg.message_id.clone(),
                    );
                    identities.record(uuid.clone());
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
                        // A goal's acceptance is the stdin hand-off; Claude's
                        // own verdict on it arrives as frames.
                        if let Some(tx) = msg.acceptance {
                            let _ = tx.send(if accepted {
                                crate::SteerAcceptance::Accepted
                            } else {
                                crate::SteerAcceptance::Rejected(
                                    "Claude input channel closed before dispatch".into(),
                                )
                            });
                        }
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

            _ = async {
                match switch_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            }, if switch_deadline.is_some() => {
                if let live_session::LineOutcome::SwitchFailed(events) =
                    live.switch_timed_out(&stdin_tx)
                {
                    steering_open = false;
                    for ev in events {
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main;
                        }
                    }
                }
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

            Some(call) = luna_tools::next_call(&mut luna_endpoint) => {
                if let Some(luna) = luna.as_mut() {
                    luna.on_call(call);
                }
            },

            _ = luna_tools::parked_deadline(luna_deadline) => {
                if let Some(luna) = luna.as_mut() {
                    luna.expire();
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

    if let Some(registration) = &live_registration {
        live_children.deregister(registration);
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

/// Events for one batch of transport receipts: exact explicit-steer
/// incorporation (`Steered` with the LUNA message id, once per input) and the
/// CC-02 turn ledger's view of each `command_lifecycle` transition.
fn receipt_events(
    norm: &mut Normalizer,
    effects: transport::Effects,
    explicit_steer_unconfirmed: &mut bool,
) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    for user_message_id in effects.incorporated {
        // Exact native receipt: this steer's uuid reached the conversation.
        *explicit_steer_unconfirmed = false;
        let (prev, next) = norm.rotate_for_steer();
        out.push(AgentEvent::Steered {
            user_message_id: Some(user_message_id),
            assistant_message_id: Some(prev),
            next_assistant_message_id: Some(next),
        });
    }
    for (uuid, state) in effects.lifecycle {
        out.extend(norm.native_input_lifecycle(&uuid, state.as_str()));
    }
    out
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
