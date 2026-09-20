//! Codex harness: spawns the installed `codex` CLI as `codex app-server` and
//! speaks JSON-RPC 2.0 over stdio — the same interface the Codex IDE extension
//! uses. Resurrected from the pre-ACP driver and modernized.
//!
//! VERSION PIN: the app-server API is EXPERIMENTAL (`capabilities.
//! experimentalApi`); this driver is validated against codex-cli 0.154.0,
//! including `contextCompaction` and imageGeneration's savedPath-only schema.
//! Revalidate the method/notification surface when bumping past it.
//!
//! - `initialize` handshake (clientInfo + `capabilities.experimentalApi`) then
//!   the `initialized` notification; unknown notification methods tolerated.
//! - `thread/start` (or `thread/resume` with a fresh-start fallback) →
//!   `SessionStarted`; `turn/start` carries the prompt, model, effort,
//!   `sandboxPolicy`, and approval policy.
//! - Notifications map to [`AgentEvent`]s: agentMessage/reasoning deltas (both
//!   `delta`/`textDelta` spellings), item lifecycles → typed ToolCall/ToolResult,
//!   `thread/tokenUsage/updated` → Usage, turn/completed|failed|aborted → Done.
//! - Approvals + sandbox follow the engine-resolved Ask / Auto / Full policy:
//!   workspace-write + user review, workspace-write + `auto_review`, or
//!   danger-full-access + `never`. Stray
//!   `item/commandExecution/requestApproval` +
//!   `item/fileChange/requestApproval` still round-trip through
//!   [`RunControls::request_input`] as a typed, machine-scoped approval.
//! - Subagents are full child app-server threads. Parent spawn items establish
//!   their stable ownership; content arriving before the spawn is buffered.
//!   A registered child's notifications route through an EXPLICIT table
//!   ([`normalize::route_child_notification`]) — content, errors and child turns
//!   become tagged [`AgentEvent::Subagent`] events; unrelated child bookkeeping is
//!   consumed so it can never settle the parent turn, and unknown methods
//!   fall through to the parent path (fail open, never silent loss).
//! - Steering: `turn/steer { expectedTurnId }` into the live turn; a rejected
//!   steer (the turn-completed race) is queued and delivered as the next
//!   `turn/start` on the same thread. The session is persistent across turns
//!   while the steering mailbox lives.
//! - Interrupt: cancelling [`RunControls::interrupt`] sends `turn/interrupt`,
//!   escalating to SIGTERM → SIGKILL if the child is unresponsive; the stream
//!   always ends with `Done { status: Interrupted }`.

pub(crate) mod catalog;
pub mod native;
mod normalize;
mod subagents;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ModelOption, ModelOptionChoice,
    ProviderForkCapability, ProviderForkRequest, ProviderForkResult, ReasoningLevel, RunRequest,
    SlashCommand, SteeringMode, UserInputAnswer, UserInputQuestion,
};

use crate::jsonrpc::{Incoming, RpcClient, RpcRequestFailure};
use crate::process::{Child, Command, Stdio};
use crate::{Harness, HarnessError, RunControls, SteerAcceptance};
use catalog::{REASONING_LEVELS, sandbox_mode, sandbox_policy_value, static_models, to_effort};
use normalize::{
    ChildRoute, ContextCompactionStream, Phase, ReasoningStream, delta_text, item_id, item_type,
    notification_thread_id, route_child_notification, turn_error_message, turn_id, usage_event,
};

/// Engine-only model-option keys. They are consumed by this adapter and never
/// forwarded as model configuration.
pub const LUNA_NATIVE_THREAD_NAME_OPTION: &str = "lunaNativeThreadName";
pub const LUNA_STRICT_RESUME_OPTION: &str = "lunaStrictResume";
pub const LUNA_CODEX_HOME_OPTION: &str = "lunaCodexHome";
pub const LUNA_CODEX_FILE_AUTH_OPTION: &str = "lunaCodexFileAuth";
const CODEX_FILE_AUTH_CONFIG: &str = "cli_auth_credentials_store=\"file\"";

/// Locate the device's installed Codex CLI: `CODEX_EXECUTABLE`, then our own
/// PATH, then the login-shell PATH snapshot (the user's shell init shapes
/// PATH in ways a GUI/service launch never sees — see [`crate::shell_env`]),
/// then known install locations as a last resort. Resolved per call — cheap
/// after the snapshot is cached.
pub fn resolve_codex_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CODEX_EXECUTABLE").filter(|p| !p.is_empty()) {
        return crate::executable::validate_native_override(&PathBuf::from(p)).ok();
    }
    let mut extra = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        extra.push(home.join(".local").join("bin").join("codex"));
        extra.push(home.join(".codex").join("bin").join("codex"));
        extra.push(home.join(".npm-global").join("bin").join("codex"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin/codex"));
    extra.push(PathBuf::from("/usr/local/bin/codex"));
    crate::executable::find_on_paths("codex", extra)
}

/// A ready-to-spawn `codex login` command for the engine's account flow.
///
/// Shares the harness's full resolution (`CODEX_EXECUTABLE`, PATH, login-shell
/// snapshot, install locations — including the Windows npm payload layout) and
/// its child-PATH composition, so "Add account" launches exactly the binary
/// the harness itself would run. `CODEX_HOME` isolates the login from the live
/// `~/.codex` session; the caller owns stdio wiring and cancellation.
pub fn login_command(codex_home: &std::path::Path) -> Result<Command, HarnessError> {
    let exe = CodexHarness::new().resolve_executable()?;
    Ok(named_command(&exe, "login", codex_home))
}

fn named_command(exe: &std::path::Path, subcommand: &str, codex_home: &std::path::Path) -> Command {
    let mut cmd = Command::new(&exe);
    crate::compose_child_path(&mut cmd, &exe);
    // `-c` is a documented global override and must precede the subcommand.
    // This is the boundary that keeps named account auth in private auth.json.
    apply_file_auth_override(&mut cmd);
    cmd.arg(subcommand).env("CODEX_HOME", codex_home);
    cmd
}

pub(super) fn apply_file_auth_override(cmd: &mut Command) {
    cmd.arg("-c").arg(CODEX_FILE_AUTH_CONFIG);
}

/// The Codex harness. Construct with [`CodexHarness::new`]; tests point it at a
/// fake app server with [`CodexHarness::with_executable`].
pub struct CodexHarness {
    executable: Option<PathBuf>,
    /// Grace between `turn/interrupt` and SIGTERM.
    interrupt_grace: Duration,
    /// Grace between SIGTERM and SIGKILL.
    kill_grace: Duration,
    /// Command discovery cache: only a successful probe is cached, so a
    /// broken CLI retries on the next picker open (ACP-harness parity).
    commands: tokio::sync::OnceCell<Vec<SlashCommand>>,
}

impl Default for CodexHarness {
    fn default() -> Self {
        Self {
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            commands: tokio::sync::OnceCell::new(),
        }
    }
}

impl CodexHarness {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a fixed CLI binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
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
        if let Some(p) = std::env::var_os("CODEX_EXECUTABLE")
            && !p.is_empty()
        {
            return crate::executable::validate_native_override(&PathBuf::from(p));
        }
        resolve_codex_executable().ok_or_else(|| {
            HarnessError::NotInstalled(
                "codex (searched PATH, the login shell's PATH, ~/.local/bin, \
                 ~/.codex/bin, ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, \
                 and fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE \
                 and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; set CODEX_EXECUTABLE \
                 to override)"
                    .into(),
            )
        })
    }

    /// Short-lived discovery probe: a `codex app-server` handshake followed by
    /// `skills/list` — the only invocable-listing method the 0.146.x wire has
    /// (custom `~/.codex/prompts` are NOT exposed; the TUI-only built-ins
    /// aren't either). Skills are what the codex TUI itself surfaces as
    /// slash-invocables, listed per-cwd and deduped by name here.
    async fn discover_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = Command::new(&exe);
        cmd.arg("app-server");
        crate::compose_child_path(&mut cmd, &exe);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("codex child has no stdio".into()));
        };
        // The receiver must stay alive for the client's reader loop; agent →
        // client traffic during the probe is ignored.
        let (client, _incoming) = RpcClient::new(stdin, stdout);
        let discovery = async {
            client
                .request(
                    "initialize",
                    json!({
                        "clientInfo": {
                            "name": "zeron-native",
                            "title": "Zeron",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "capabilities": { "experimentalApi": true },
                    }),
                )
                .await?;
            client.notify("initialized", None);
            let skills = client.request("skills/list", json!({})).await?;
            Ok::<Vec<SlashCommand>, HarnessError>(parse_skill_commands(&skills))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }

    /// Short-lived live catalog probe. `model/list` is paginated and already
    /// applies the signed-in account's rollout/visibility policy, so hidden or
    /// unavailable models (including staged Astra rollouts) never leak into a
    /// successful picker response.
    async fn discover_models(
        &self,
        codex_home: Option<&std::path::Path>,
        force_file_auth: bool,
    ) -> Result<Vec<Model>, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = Command::new(&exe);
        if force_file_auth {
            apply_file_auth_override(&mut cmd);
        }
        cmd.arg("app-server");
        if let Some(codex_home) = codex_home {
            cmd.env("CODEX_HOME", codex_home);
        }
        crate::compose_child_path(&mut cmd, &exe);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("codex child has no stdio".into()));
        };
        let (client, _incoming) = RpcClient::new(stdin, stdout);
        let discovery = async {
            client
                .request(
                    "initialize",
                    json!({
                        "clientInfo": {
                            "name": "zeron-native",
                            "title": "Zeron",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "capabilities": { "experimentalApi": true },
                    }),
                )
                .await?;
            client.notify("initialized", None);

            let mut models = Vec::new();
            let mut model_ids = HashSet::new();
            let mut seen_cursors = HashSet::new();
            let mut cursor: Option<String> = None;
            let mut default_model_id: Option<String> = None;
            loop {
                let mut params = json!({ "limit": 20, "includeHidden": false });
                if let Some(cursor) = cursor.as_deref() {
                    params["cursor"] = Value::String(cursor.to_owned());
                }
                let page = client.request("model/list", params).await?;
                let (page_models, next_cursor) = parse_model_list_page(&page);
                for (model, is_default) in page_models {
                    if model_ids.insert(model.id.clone()) {
                        if is_default && default_model_id.is_none() {
                            default_model_id = Some(model.id.clone());
                        }
                        models.push(model);
                    }
                }
                let Some(next) = next_cursor.filter(|next| !next.is_empty()) else {
                    break;
                };
                if !seen_cursors.insert(next.clone()) {
                    break;
                }
                cursor = Some(next);
            }

            if let Some(default_id) = default_model_id
                && let Some(index) = models.iter().position(|model| model.id == default_id)
                && index != 0
            {
                let default_model = models.remove(index);
                models.insert(0, default_model);
            }
            Ok::<Vec<Model>, HarnessError>(models)
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("model discovery timed out".into())),
        }
    }
}

fn reasoning_level(value: &str) -> Option<ReasoningLevel> {
    Some(match value {
        "minimal" => ReasoningLevel::Minimal,
        "low" => ReasoningLevel::Low,
        "medium" => ReasoningLevel::Medium,
        "high" => ReasoningLevel::High,
        "xhigh" => ReasoningLevel::XHigh,
        "max" => ReasoningLevel::Max,
        "ultra" => ReasoningLevel::Ultra,
        "ultracode" => ReasoningLevel::Ultracode,
        "ultrathink" => ReasoningLevel::Ultrathink,
        _ => return None,
    })
}

/// Codex accepts both names, but Zeron has historically persisted `fast`.
/// Normalize the app server's `priority` id so live and fallback catalogs do
/// not produce two different settings for the same tier.
fn normalized_service_tier(value: &str) -> &str {
    match value {
        "priority" => "fast",
        other => other,
    }
}

fn service_tier_label(value: &str) -> String {
    match value {
        "fast" | "priority" => "Fast".into(),
        "flex" => "Flex".into(),
        "ultrafast" => "Ultra Fast".into(),
        other => other.to_owned(),
    }
}

fn model_service_tier(item: &Value) -> Option<ModelOption> {
    let mut choices = vec![ModelOptionChoice {
        id: "default".into(),
        label: "Standard".into(),
    }];
    let mut seen = HashSet::from(["default".to_owned()]);
    for tier in item
        .get("serviceTiers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(wire_id) = tier.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = normalized_service_tier(wire_id).to_owned();
        if !seen.insert(id.clone()) {
            continue;
        }
        let label = tier
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| service_tier_label(wire_id));
        choices.push(ModelOptionChoice { id, label });
    }
    for tier in item
        .get("additionalSpeedTiers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(wire_id) = tier.as_str() else {
            continue;
        };
        let id = normalized_service_tier(wire_id).to_owned();
        if seen.insert(id.clone()) {
            choices.push(ModelOptionChoice {
                id,
                label: service_tier_label(wire_id),
            });
        }
    }
    if choices.len() == 1 {
        return None;
    }
    let default_choice = item
        .get("defaultServiceTier")
        .and_then(Value::as_str)
        .map(normalized_service_tier)
        .filter(|id| seen.contains(*id))
        .unwrap_or("default")
        .to_owned();
    Some(ModelOption {
        id: "serviceTier".into(),
        label: "Service Tier".into(),
        choices,
        default_choice,
    })
}

/// Parse one `model/list` page. Unknown future reasoning levels are ignored
/// independently instead of invalidating the complete catalog.
fn parse_model_list_page(result: &Value) -> (Vec<(Model, bool)>, Option<String>) {
    let mut models = Vec::new();
    for item in result
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if item.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(id) = item
            .get("model")
            .and_then(Value::as_str)
            .or_else(|| item.get("id").and_then(Value::as_str))
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let label = item
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(id)
            .to_owned();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_owned);
        let reasoning_levels = item
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|effort| {
                effort
                    .get("reasoningEffort")
                    .and_then(Value::as_str)
                    .or_else(|| effort.as_str())
                    .and_then(reasoning_level)
            })
            .collect();
        let options = model_service_tier(item).into_iter().collect();
        models.push((
            Model {
                id: id.to_owned(),
                label,
                description,
                reasoning_levels,
                options,
            },
            item.get("isDefault").and_then(Value::as_bool) == Some(true),
        ));
    }
    let next_cursor = result
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    (models, next_cursor)
}

/// `skills/list` result → picker commands. `data` groups skills by cwd; the
/// same skill appears under every root, so dedupe by name keeping first
/// appearance order. The interface's shortDescription is picker-sized; the
/// top-level description is a model-facing paragraph, kept only as fallback.
fn parse_skill_commands(result: &Value) -> Vec<SlashCommand> {
    let mut seen = std::collections::HashSet::new();
    let mut commands = Vec::new();
    for group in result
        .get("data")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
    {
        for skill in group
            .get("skills")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
        {
            let Some(name) = skill
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            if !seen.insert(name.to_owned()) {
                continue;
            }
            let interface = skill.get("interface");
            let description = interface
                .and_then(|i| i.get("shortDescription"))
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .or_else(|| skill.get("description").and_then(Value::as_str))
                .unwrap_or_default();
            commands.push(SlashCommand {
                name: name.to_owned(),
                description: description.to_owned(),
                input_hint: None,
            });
        }
    }
    commands
}

#[async_trait]
impl Harness for CodexHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }
    fn display_name(&self) -> &str {
        // "Codex" (not "Codex CLI") — comet composer/defaults.ts
        // HARNESS_LABEL; must also match the registry's lazy descriptor so
        // the catalog entry doesn't change after the first resolve.
        "Codex"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    /// Native `turn/steer` injects into the active turn; a steer that misses
    /// the turn falls back to a follow-up `turn/start` on the same thread.
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        REASONING_LEVELS
    }
    fn installed(&self) -> bool {
        self.resolve_executable().is_ok()
    }
    /// Done is the CLI's own terminal frame, for wake turns too.
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    /// Managed Codex task resumes are strict: the engine supplies
    /// `lunaStrictResume`, and this adapter returns the resume error instead of
    /// silently starting a fresh thread. That is the no-duplicate contract L10
    /// needs for accepted-turn continuation.
    fn supports_safe_recovery_continuation(&self) -> bool {
        true
    }

    fn fork_capability(&self) -> ProviderForkCapability {
        ProviderForkCapability::Checkpoint
    }

    async fn fork_session(
        &self,
        request: ProviderForkRequest,
    ) -> Result<ProviderForkResult, HarnessError> {
        let executable = self.resolve_executable()?;
        let home = request
            .provider_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .or_else(|| crate::executable::home_dir().map(|home| home.join(".codex")))
            .ok_or_else(|| HarnessError::Protocol("Codex home is unavailable".into()))?;
        let client = native::CodexNativeClient::new(&home)
            .with_executable(executable)
            .for_execution_home(home, request.force_file_auth);
        client.fork_thread(request).await
    }

    /// The signed-in account's visible `model/list` is authoritative. A
    /// curated snapshot keeps the picker operational when the experimental
    /// discovery call is unavailable or temporarily fails; failed probes are
    /// intentionally not cached so reopening the picker retries rollout state.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        match self.discover_models(None, false).await {
            Ok(models) if !models.is_empty() => Ok(models),
            Ok(_) => Ok(static_models()),
            Err(error) => {
                tracing::debug!(
                    target: "zeron_harness::codex",
                    "model/list discovery failed; using fallback catalog: {error}"
                );
                Ok(static_models())
            }
        }
    }

    /// Skills from a short-lived `skills/list` probe (see
    /// [`Self::discover_commands`]); cached on success.
    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        self.commands
            .get_or_try_init(|| self.discover_commands())
            .await
            .cloned()
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
        let codex_home = request.model_options.remove(LUNA_CODEX_HOME_OPTION);
        let codex_file_auth = request.model_options.remove(LUNA_CODEX_FILE_AUTH_OPTION);
        request.resume = None;
        request.worktree = None;
        request.attachments.clear();
        request.model_options.clear();
        if let Some(codex_home) = codex_home {
            request
                .model_options
                .insert(LUNA_CODEX_HOME_OPTION.into(), codex_home);
        }
        if let Some(codex_file_auth) = codex_file_auth {
            request
                .model_options
                .insert(LUNA_CODEX_FILE_AUTH_OPTION.into(), codex_file_auth);
        }
        request.auto_approve = false;
        self.run_with_mode(request, controls, true).await
    }
}

impl CodexHarness {
    /// Account-scoped model discovery. The home is resolved by the engine from
    /// a trusted account id; it is never accepted directly from the UI.
    pub async fn models_for_home(
        &self,
        codex_home: &std::path::Path,
        force_file_auth: bool,
    ) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        match self
            .discover_models(Some(codex_home), force_file_auth)
            .await
        {
            Ok(models) if !models.is_empty() => Ok(models),
            Ok(_) => Ok(static_models()),
            Err(error) => {
                tracing::debug!(
                    target: "zeron_harness::codex",
                    "account-scoped model/list discovery failed; using fallback catalog: {error}"
                );
                Ok(static_models())
            }
        }
    }

    async fn run_with_mode(
        &self,
        mut request: RunRequest,
        controls: RunControls,
        title_only: bool,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let exe = self.resolve_executable()?;
        // Title generation has no tools and remains read-only. Ordinary runs
        // keep the engine-resolved sandbox; never broaden it in the adapter.
        if title_only {
            request.sandbox = zeron_proto::SandboxLevel::ReadOnly;
        }
        let mut cmd = Command::new(&exe);
        let force_file_auth = request
            .model_options
            .remove(LUNA_CODEX_FILE_AUTH_OPTION)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if force_file_auth {
            apply_file_auth_override(&mut cmd);
        }
        cmd.arg("app-server");
        if let Some(codex_home) = request
            .model_options
            .remove(LUNA_CODEX_HOME_OPTION)
            .and_then(|value| value.as_str().map(std::path::PathBuf::from))
        {
            cmd.env("CODEX_HOME", codex_home);
        }
        request
            .model_options
            .remove(crate::LUNA_PROVIDER_AUTH_INSTANCE_OPTION);
        crate::compose_child_path(&mut cmd, &exe);
        if !request.cwd.is_empty() {
            cmd.current_dir(&request.cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
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
            .ok_or_else(|| HarnessError::Protocol("codex child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("codex child has no stdout".into()))?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "zeron_harness::codex", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (client, incoming) = RpcClient::new(stdin, stdout);
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            title_only,
            child,
            client,
            incoming,
            event_tx,
            controls,
            request,
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

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

struct Session {
    title_only: bool,
    child: Child,
    client: RpcClient,
    incoming: mpsc::Receiver<Incoming>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    request: RunRequest,
    interrupt_grace: Duration,
    kill_grace: Duration,
    /// Rolling stderr tail for the crash message on an unexpected exit.
    stderr_tail: crate::StderrTail,
}

/// Turn-routing state (port of codex.ts's activeTurnId/completedTurnIds): the
/// `turn/start` response and the turn lifecycle notifications are separate
/// app-server messages that may arrive in either order — never revive a turn
/// that `turn/completed` already declared finished.
#[derive(Default)]
struct TurnRouter {
    active: Option<String>,
    completed: VecDeque<String>,
}

impl TurnRouter {
    fn is_completed(&self, id: &str) -> bool {
        self.completed.iter().any(|c| c == id)
    }

    fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }

    fn note_started(&mut self, id: String) {
        if id.is_empty() || self.is_completed(&id) {
            return;
        }
        // A replacement `turn/started` is authoritative evidence that a stale
        // active turn is over, even if its completion notification was lost.
        if let Some(prev) = self.active.take()
            && prev != id
        {
            self.remember_completed(prev);
        }
        self.active = Some(id);
    }

    fn note_completed(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.remember_completed(id.to_owned());
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
    }

    /// Adopt a turn id from a `turn/start` RESPONSE (the notification is
    /// allowed to beat it).
    fn adopt_started(&mut self, id: String) {
        self.active = (!id.is_empty() && !self.is_completed(&id)).then_some(id);
    }

    fn remember_completed(&mut self, id: String) {
        self.completed.push_back(id);
        // Bounded so a months-long persistent session can't grow it forever.
        while self.completed.len() > 32 {
            self.completed.pop_front();
        }
    }
}

fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Rotate the assistant message id; returns (previous, next).
fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

fn clear_explicit_steer_turn(owner: &mut Option<String>, terminal_turn_id: &str) {
    if owner.as_deref() == Some(terminal_turn_id) {
        *owner = None;
    }
}

async fn send(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

/// `turn/start` and return the new turn id from the response.
async fn start_turn(client: &RpcClient, params: Value) -> Result<String, HarnessError> {
    let started = client.request("turn/start", params).await?;
    Ok(started["turn"]["id"].as_str().unwrap_or("").to_owned())
}

/// The per-run event loop: one task multiplexing app-server messages, the
/// steering mailbox, the interrupt token, and consumer liveness.
async fn run_session(session: Session) {
    let Session {
        title_only,
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
        interrupt_grace,
        kill_grace,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
    } = controls;
    let request_input = Arc::new(request_input);

    // ---- wire params ------------------------------------------------------
    let effective_mode = request
        .permission_policy
        .as_ref()
        .map(|policy| policy.effective_mode)
        .unwrap_or(zeron_proto::PermissionMode::Ask);
    let (approval_policy, approvals_reviewer) =
        codex_permission_settings(effective_mode, title_only);
    let effort = to_effort(request.reasoning);
    // Service tier rides thread-start and every turn (mirrors the Codex IDE
    // client). "default" means Standard — omit it entirely.
    let service_tier = request
        .model_options
        .get("serviceTier")
        .and_then(Value::as_str)
        .filter(|t| *t != "default")
        .map(str::to_owned);

    let start_params = {
        let mut p = serde_json::Map::new();
        if title_only {
            p.insert("baseInstructions".into(), crate::TITLE_INSTRUCTIONS.into());
            p.insert(
                "developerInstructions".into(),
                crate::TITLE_INSTRUCTIONS.into(),
            );
            p.insert("ephemeral".into(), true.into());
            p.insert(
                "config".into(),
                json!({
                    "project_doc_max_bytes": 0,
                    "web_search": "disabled",
                    "features.shell_tool": false,
                    "features.apply_patch_freeform": false,
                    "features.multi_agent": false,
                    "features.apps": false,
                    "features.multi_agent_v2": false,
                    "agents.enabled": false,
                    "features.browser_use": false,
                    "features.computer_use": false,
                    "features.js_repl": false,
                    "features.image_generation": false,
                    "features.memories": false
                }),
            );
        } else {
            // Ordinary LUNA work must materialize as a provider-native thread.
            p.insert("ephemeral".into(), false.into());
        }
        p.insert("cwd".into(), Value::String(request.cwd.clone()));
        p.insert("approvalPolicy".into(), approval_policy.into());
        p.insert("approvalsReviewer".into(), approvals_reviewer.into());
        p.insert("sandbox".into(), sandbox_mode(request.sandbox).into());
        if let Some(model) = &request.model {
            p.insert("model".into(), Value::String(model.clone()));
        }
        if let Some(tier) = &service_tier {
            p.insert("serviceTier".into(), Value::String(tier.clone()));
        }
        p
    };

    // ---- handshake + thread + first turn (interruptible) ------------------
    let setup = async {
        client
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "zeron-native",
                        "title": "Zeron",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": { "experimentalApi": true },
                }),
            )
            .await?;
        client.notify("initialized", None);

        let mut start_params = start_params.clone();
        if title_only {
            // Disable each configured MCP server explicitly: an empty table
            // would merge with user configuration and leave servers enabled.
            let config = client
                .request("config/read", json!({"includeLayers": false}))
                .await?;
            if let Some(servers) = config["config"]["mcp_servers"].as_object() {
                let overrides = start_params
                    .get_mut("config")
                    .and_then(Value::as_object_mut)
                    .unwrap();
                for name in servers.keys() {
                    overrides.insert(format!("mcp_servers.{name}.enabled"), false.into());
                }
            }
        }
        let strict_resume = request
            .model_options
            .get(LUNA_STRICT_RESUME_OPTION)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let thread = if let Some(resume) = &request.resume {
            let mut p = start_params.clone();
            p.insert("threadId".into(), Value::String(resume.clone()));
            match client.request("thread/resume", Value::Object(p)).await {
                Ok(thread) => thread,
                // A missing/foreign rollout falls back to a fresh thread.
                Err(e) if !strict_resume => {
                    tracing::debug!(
                        target: "zeron_harness::codex",
                        "thread/resume failed (starting fresh): {e}"
                    );
                    client
                        .request("thread/start", Value::Object(start_params.clone()))
                        .await?
                }
                Err(e) => return Err(e),
            }
        } else {
            client
                .request("thread/start", Value::Object(start_params.clone()))
                .await?
        };
        let thread_id = thread["thread"]["id"].as_str().unwrap_or("").to_owned();
        if !title_only
            && let Some(name) = request
                .model_options
                .get(LUNA_NATIVE_THREAD_NAME_OPTION)
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
            && let Err(error) = client
                .request(
                    "thread/name/set",
                    json!({"threadId": thread_id, "name": name}),
                )
                .await
        {
            // The durable thread still exists and must be surfaced to the
            // engine so it can bind/reconcile it instead of orphaning it.
            tracing::warn!(target: "zeron_harness::codex", %thread_id, %error, "native thread name set failed");
        }
        let mut children = subagents::Subagents::new(thread_id.clone());
        children.restore(&thread["thread"]);
        Ok::<_, HarnessError>((thread_id, children))
    };
    let (thread_id, mut children) = tokio::select! {
        res = setup => match res {
            Ok(thread_id) => thread_id,
            Err(e) => {
                let _ = event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: None,
                    }))
                    .await;
                shutdown_child(&mut child, kill_grace).await;
                return;
            }
        },
        _ = interrupt.cancelled() => {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                }))
                .await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };

    let turn_params = |text: &str| -> Value {
        let mut p = serde_json::Map::new();
        p.insert("threadId".into(), Value::String(thread_id.clone()));
        p.insert("input".into(), json!([{ "type": "text", "text": text }]));
        p.insert("approvalPolicy".into(), approval_policy.into());
        p.insert("approvalsReviewer".into(), approvals_reviewer.into());
        p.insert(
            "sandboxPolicy".into(),
            sandbox_policy_value(request.sandbox),
        );
        // Reasoning summaries stream (`item/reasoning/summaryTextDelta`) only
        // when asked for — without this codex "thinks" in silence for minutes:
        // nothing renders and the UI's 45s staleness gate flips Working off
        // (user report: "not streaming, doesn't say it's working").
        p.insert("summary".into(), "auto".into());
        if let Some(model) = &request.model {
            p.insert("model".into(), Value::String(model.clone()));
        }
        if let Some(effort) = effort {
            p.insert("effort".into(), effort.into());
        }
        if let Some(tier) = &service_tier {
            p.insert("serviceTier".into(), Value::String(tier.clone()));
        }
        Value::Object(p)
    };

    let mut assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Codex,
            model: request.model.clone().unwrap_or_default(),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: thread_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }

    let mut router = TurnRouter::default();
    match start_turn(&client, turn_params(&request.prompt)).await {
        Ok(id) => {
            router.adopt_started(id.clone());
            if !send(
                &event_tx,
                AgentEvent::ProviderCheckpoint { checkpoint_id: id },
            )
            .await
            {
                shutdown_child(&mut child, kill_grace).await;
                return;
            }
        }
        Err(e) => {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(e.to_string()),
                    session_id: Some(thread_id.clone()),
                }))
                .await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    }

    // ---- main loop --------------------------------------------------------
    // Deltas seen per agent-message item, so a model that never streams
    // (item/completed only) still emits its text exactly once.
    let mut streamed_text: HashSet<String> = HashSet::new();
    let mut reasoning_streams: HashMap<String, ReasoningStream> = HashMap::new();
    let mut context_compactions = ContextCompactionStream::default();
    // Token usage is held until the turn ends, emitted just before Done.
    let mut pending_usage: Option<AgentEvent> = None;
    // Steers whose `turn/steer` lost the turn-completed race; delivered as the
    // next `turn/start` when the expected turn's end notification arrives.
    let mut queued_steers: VecDeque<String> = VecDeque::new();
    // The Codex wire does not correlate echoed user items to `turn/steer`
    // request ids. Keep explicit steering single-flight and never manufacture
    // incorporation from a text match; a successful RPC response is only an
    // accepted-but-unconfirmed delivery.
    let mut explicit_steer_turn: Option<String> = None;
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    // A Done has been emitted for the turn currently/last in flight.
    let mut done_current = false;
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        tokio::select! {
            inc = incoming.recv() => match inc {
                Some(Incoming::Notification { method, params }) => {
                // Foreign-thread traffic FIRST: a child thread's turn/thread
                // bookkeeping must never reach the parent turn router below
                // (a child's turn/completed would settle the PARENT turn).
                if let Some(nthread) = notification_thread_id(&method, &params)
                    && !nthread.is_empty()
                    && nthread != thread_id
                {
                    match route_child_notification(&method) {
                        ChildRoute::Parent => {
                            // Unknown/parent-owned: fall through so a codex
                            // update degrades to "the parent sees it",
                            // never silent loss.
                        }
                        ChildRoute::Consumed => continue,
                        ChildRoute::Subagent => {
                            for event in children.notification(&nthread, &method, &params) {
                                if !send(&event_tx, event).await {
                                    break 'main;
                                }
                            }
                            continue;
                        }
                    }
                }
                match method.as_str() {
                    "turn/started" => {
                        let id = turn_id(&params);
                        if let Some(event) = context_compactions
                            .unknown_before_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
                        router.note_started(id);
                    }

                    "thread/name/updated"
                    | "thread/archived"
                    | "thread/unarchived"
                    | "thread/status/changed" => {
                        if !send(
                            &event_tx,
                            AgentEvent::ProviderTaskChanged {
                                change: method.clone(),
                            },
                        )
                        .await
                        {
                            break 'main;
                        }
                    }

                    "item/agentMessage/delta" => {
                        streamed_text.insert(item_id(&params));
                        if let Some(text) = delta_text(&params)
                            && !send(&event_tx, AgentEvent::TextDelta { text }).await
                        {
                            break 'main;
                        }
                    }

                    "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta"
                    | "item/reasoning/summaryPartAdded" => {
                        for event in reasoning_streams.entry(thread_id.clone()).or_default()
                            .map(&method, &params)
                        {
                            if !send(&event_tx, event).await {
                                break 'main;
                            }
                        }
                    }

                    "item/started" | "item/completed" => {
                        let phase = if method == "item/started" {
                            Phase::Started
                        } else {
                            Phase::Completed
                        };
                        let item = params.get("item").unwrap_or(&Value::Null);
                        if matches!(item_type(item), "contextCompaction" | "context_compaction") {
                            if let Some(event) = context_compactions.map_item(
                                phase,
                                &thread_id,
                                router.active_id(),
                                &params,
                                item,
                            ) && !send(&event_tx, event).await
                            {
                                break 'main;
                            }
                            continue;
                        }
                        if matches!(item_type(item), "agentMessage" | "agent_message") {
                            if phase == Phase::Completed {
                                // Fallback for non-streamed messages only.
                                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                                if !streamed_text.contains(id)
                                    && !text.is_empty()
                                    && !send(&event_tx, AgentEvent::TextDelta { text: text.into() }).await
                                {
                                    break 'main;
                                }
                                // Codex emits several assistant messages per
                                // turn (commentary, final answer); their
                                // deltas carry no separator, so consecutive
                                // messages rendered concatenated
                                // ("…waiting.Beta's 90-second…" — live
                                // finding). Close each message as a
                                // paragraph.
                                if !send(
                                    &event_tx,
                                    AgentEvent::TextDelta {
                                        text: "\n\n".into(),
                                    },
                                )
                                .await
                                {
                                    break 'main;
                                }
                                // Deltas are token chunks, not steering
                                // boundaries: the completed item is the
                                // provider-authoritative end of the text part.
                                let (prev, _next) = rotate(&mut assistant_message_id);
                                if !send(
                                    &event_tx,
                                    AgentEvent::AssistantMessageCompleted {
                                        assistant_message_id: prev,
                                    },
                                )
                                .await
                                {
                                    break 'main;
                                }
                            }
                        } else {
                            for ev in children.parent_item(phase, item) {
                                if !send(&event_tx, ev).await {
                                    break 'main;
                                }
                            }
                        }
                    }

                    "thread/tokenUsage/updated" => {
                        if let Some(usage) = normalize::context_usage_event(&params)
                            && !send(&event_tx, usage).await { break 'main; }
                        if let Some(usage) = usage_event(&params) {
                            pending_usage = Some(usage);
                        }
                    }

                    "turn/completed" => {
                        let id = turn_id(&params);
                        if let Some(event) = context_compactions
                            .unknown_for_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
                        clear_explicit_steer_turn(&mut explicit_steer_turn, &id);
                        router.note_completed(&id);
                        // Item ids never span turns; without this the set grew
                        // one entry per message for a persistent session's life.
                        streamed_text.clear();
                        if let Some(usage) = pending_usage.take()
                            && !send(&event_tx, usage).await
                        {
                            break 'main;
                        }
                        let error = turn_error_message(&params).or_else(|| {
                            (params
                                .pointer("/turn/status")
                                .and_then(Value::as_str)
                                == Some("failed"))
                            .then(|| "Codex turn failed".to_owned())
                        });
                        let status = if interrupted {
                            DoneStatus::Interrupted
                        } else if error.is_some() {
                            DoneStatus::Errored
                        } else {
                            DoneStatus::Completed
                        };
                        done_current = true;
                        if !send(
                            &event_tx,
                            AgentEvent::Done {
                                status,
                                result: None,
                                error,
                                session_id: Some(thread_id.clone()),
                            },
                        )
                        .await
                        {
                            break 'main;
                        }
                        if interrupted {
                            done_after_interrupt = true;
                            break 'main;
                        }
                        // Persistent session: a steer that lost the race with
                        // this turn's end becomes the next turn now; otherwise
                        // stay alive for the mailbox — the caller owns teardown.
                        if let Some(text) = queued_steers.pop_front() {
                            if !steer_as_new_turn(
                                &client,
                                turn_params(&text),
                                &mut router,
                                &event_tx,
                                &mut assistant_message_id,
                                &mut done_current,
                            )
                            .await
                            {
                                break 'main;
                            }
                        } else if !steering_open {
                            break 'main;
                        }
                    }

                    "turn/failed" => {
                        let id = turn_id(&params);
                        if let Some(event) = context_compactions
                            .unknown_for_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
                        router.note_completed(&id);
                        if let Some(usage) = pending_usage.take()
                            && !send(&event_tx, usage).await
                        {
                            break 'main;
                        }
                        done_current = true;
                        if interrupted {
                            done_after_interrupt = true;
                        }
                        let _ = send(
                            &event_tx,
                            AgentEvent::Done {
                                status: if interrupted {
                                    DoneStatus::Interrupted
                                } else {
                                    DoneStatus::Errored
                                },
                                result: None,
                                error: Some(
                                    turn_error_message(&params)
                                        .unwrap_or_else(|| "Codex turn failed".into()),
                                ),
                                session_id: Some(thread_id.clone()),
                            },
                        )
                        .await;
                        break 'main;
                    }

                    "turn/aborted" => {
                        let id = turn_id(&params);
                        if let Some(event) = context_compactions
                            .unknown_for_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
                        router.note_completed(&id);
                        done_current = true;
                        if interrupted {
                            done_after_interrupt = true;
                        }
                        let _ = send(
                            &event_tx,
                            AgentEvent::Done {
                                status: DoneStatus::Interrupted,
                                result: None,
                                error: None,
                                session_id: Some(thread_id.clone()),
                            },
                        )
                        .await;
                        break 'main;
                    }

                    "error" => {
                        // 0.146.x nests it (`params.error.message`); older
                        // builds were flat (`params.message`) — accept both.
                        let message = params
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .or_else(|| params.get("message").and_then(Value::as_str))
                            .unwrap_or("Codex error")
                            .to_owned();
                        if !send(&event_tx, AgentEvent::Error { message }).await {
                            break 'main;
                        }
                    }

                    // thread/status, mcpServer startup, account noise, … —
                    // unknown notification methods are tolerated by design.
                    _ => {}
                }
                }

                Some(Incoming::Request { id, method, params }) => {
                    handle_server_request(
                        &client,
                        id,
                        &method,
                        &params,
                        request.auto_approve,
                        request.permission_policy.clone().unwrap_or_default(),
                        &request_input,
                    );
                }

                // stdout EOF or reader gone: the app server exited.
                Some(Incoming::Eof) | None => break 'main,
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    let text = msg.prompt;
                    let require_active =
                        msg.delivery == crate::SteerDelivery::RequireActiveTurn;
                    let mut acceptance = msg.acceptance;
                    if require_active && explicit_steer_turn.is_some() {
                        if let Some(tx) = acceptance.take() {
                            let _ = tx.send(SteerAcceptance::Rejected(
                                "another Codex steer is accepted but incorporation is unconfirmed"
                                    .into(),
                            ));
                        }
                        continue 'main;
                    }
                    if let Some(expected) = router.active.clone() {
                        let steer_params = json!({
                            "threadId": thread_id,
                            "expectedTurnId": expected,
                            "input": [{ "type": "text", "text": text }],
                        });
                        match client.request_classified("turn/steer", steer_params).await {
                            Ok(_) => {
                                if require_active {
                                    explicit_steer_turn = Some(expected.clone());
                                    if let Some(tx) = acceptance.take() {
                                        let _ = tx.send(SteerAcceptance::Accepted);
                                    }
                                } else {
                                    // Ordinary warm routing retains its legacy
                                    // segment boundary policy. It has no durable
                                    // explicit-steer receipt to settle.
                                    let (prev, next) = rotate(&mut assistant_message_id);
                                    if !send(
                                        &event_tx,
                                        AgentEvent::Steered {
                                            assistant_message_id: Some(prev),
                                            next_assistant_message_id: Some(next),
                                        },
                                    )
                                    .await
                                    {
                                        break 'main;
                                    }
                                }
                            }
                            // A failed `turn/steer` does NOT mean the text is
                            // bad: most commonly the active turn finished
                            // between the UI send and this request. Queue it
                            // for redelivery as the next `turn/start` when the
                            // expected turn's end arrives (also the safe
                            // fallback for older Codex without steering).
                            Err(failure) => {
                                if require_active {
                                    if let Some(tx) = acceptance.take() {
                                        let outcome = match failure {
                                            RpcRequestFailure::Rejected(message) => {
                                                SteerAcceptance::Rejected(message)
                                            }
                                            RpcRequestFailure::Unknown(message) => {
                                                SteerAcceptance::Unknown(message)
                                            }
                                        };
                                        let _ = tx.send(outcome);
                                    }
                                    continue 'main;
                                }
                                tracing::debug!(
                                    target: "zeron_harness::codex",
                                    "turn/steer did not settle (queued as next turn): {}",
                                    failure.message()
                                );
                                if router.active.as_deref() == Some(expected.as_str())
                                    && !router.is_completed(&expected)
                                {
                                    queued_steers.push_back(text);
                                } else if !steer_as_new_turn(
                                    &client,
                                    turn_params(&text),
                                    &mut router,
                                    &event_tx,
                                    &mut assistant_message_id,
                                    &mut done_current,
                                )
                                .await
                                {
                                    break 'main;
                                }
                            }
                        }
                    } else if require_active {
                        if let Some(tx) = acceptance.take() {
                            let _ = tx.send(SteerAcceptance::Rejected(
                                "no active Codex turn to steer".into(),
                            ));
                        }
                    } else if !steer_as_new_turn(
                        &client,
                        turn_params(&text),
                        &mut router,
                        &event_tx,
                        &mut assistant_message_id,
                        &mut done_current,
                    )
                    .await
                    {
                        break 'main;
                    }
                }
                None => {
                    // Mailbox closed (the caller's graceful idle-reap): finish
                    // once nothing is in flight — mirrors codex.ts's steer loop
                    // `finish()` on a null take.
                    steering_open = false;
                    if router.active.is_none() && queued_steers.is_empty() {
                        break 'main;
                    }
                }
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                if let Some(turn) = router.active.clone() {
                    let client = client.clone();
                    let thread = thread_id.clone();
                    tokio::spawn(async move {
                        if let Err(e) = client
                            .request("turn/interrupt", json!({ "threadId": thread, "turnId": turn }))
                            .await
                        {
                            tracing::debug!(
                                target: "zeron_harness::codex",
                                "turn/interrupt failed (escalation will reap): {e}"
                            );
                        }
                    });
                    // Escalate if the app server doesn't wind down (turn/aborted)
                    // within the grace periods: SIGTERM, then SIGKILL.
                    if let Some(pid) = crate::process::signal_target(&child) {
                        escalation = Some(tokio::spawn(async move {
                            tokio::time::sleep(interrupt_grace).await;
                            send_signal(&pid, Signal::Term);
                            tokio::time::sleep(kill_grace).await;
                            send_signal(&pid, Signal::Kill);
                        }));
                    }
                } else {
                    // Idle between turns: nothing to interrupt — the terminal
                    // bookkeeping below still guarantees Done { Interrupted }.
                    break 'main;
                }
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    // Terminal bookkeeping: never end the stream without a Done unless the
    // consumer already hung up.
    if !event_tx.is_closed() {
        if let Some(event) = context_compactions.take_unknown() {
            let _ = event_tx.send(Ok(event)).await;
        }
        if interrupted && !done_after_interrupt {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(thread_id.clone()),
                }))
                .await;
        } else if !interrupted && !done_current {
            // A child KILLED mid-turn (OS memory pressure, `killall codex`)
            // must not read as a silent success — codex.ts's signal-death
            // handling, reduced to the turn-in-flight case.
            let status = child.try_wait().ok().flatten();
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message(
                        "codex app-server",
                        status,
                        &stderr_tail,
                    )),
                    session_id: Some(thread_id.clone()),
                }))
                .await;
        }
    }

    shutdown_child(&mut child, kill_grace).await;
    if let Some(handle) = escalation {
        handle.abort();
    }
}

fn codex_permission_settings(
    mode: zeron_proto::PermissionMode,
    title_only: bool,
) -> (&'static str, &'static str) {
    if title_only {
        return ("never", "user");
    }
    match mode {
        zeron_proto::PermissionMode::Ask => ("on-request", "user"),
        zeron_proto::PermissionMode::Auto => ("on-request", "auto_review"),
        zeron_proto::PermissionMode::Full => ("never", "user"),
    }
}

/// Deliver a steer as a fresh `turn/start` on the same thread (the fallback
/// leg of the steer race, and the between-turns delivery path). Returns false
/// when the loop should end (turn/start failed or the consumer hung up).
async fn steer_as_new_turn(
    client: &RpcClient,
    params: Value,
    router: &mut TurnRouter,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    assistant_message_id: &mut String,
    done_current: &mut bool,
) -> bool {
    match start_turn(client, params).await {
        Ok(id) => {
            router.adopt_started(id.clone());
            *done_current = false;
            let (prev, next) = rotate(assistant_message_id);
            if !send(
                event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return false;
            }
            send(
                event_tx,
                AgentEvent::ProviderCheckpoint { checkpoint_id: id },
            )
            .await
        }
        Err(e) => {
            let _ = send(
                event_tx,
                AgentEvent::Error {
                    message: format!("Steering failed: {e}"),
                },
            )
            .await;
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Approvals (approval-as-input parity with zeron's UX)
// ---------------------------------------------------------------------------

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

/// Serve one server→client request. Approval requests round-trip through
/// `request_input` as a synthesized yes/no question (in a subtask so the
/// message loop keeps flowing); with `auto_approve` they're accepted outright
/// (belt to the wire-level `approvalPolicy: "never"`). Anything else is
/// rejected as unsupported so the server never wedges awaiting a reply.
fn handle_server_request(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
    auto_approve: bool,
    permission_policy: zeron_proto::PermissionPolicy,
    request_input: &Arc<RequestInputFn>,
) {
    // A tool's user-input request (EXPERIMENTAL, codex 0.146.x) is a CONTENT
    // question, never auto-approvable — route it to the input bridge and
    // answer keyed by question id, `{ answers: { <id>: { answers: [..] } } }`.
    if method == "item/tool/requestUserInput" {
        let questions = user_input_questions(params);
        if questions.is_empty() {
            client.respond(&id, json!({ "answers": {} }));
            return;
        }
        let client = client.clone();
        let request_input = Arc::clone(request_input);
        tokio::spawn(async move {
            let asked: Vec<UserInputQuestion> = questions.iter().map(|(_, q)| q.clone()).collect();
            let answers = (request_input)(asked).await.unwrap_or_default();
            let mut by_id = serde_json::Map::new();
            for (wire_id, q) in &questions {
                let labels: Vec<Value> = answers
                    .iter()
                    .find(|a| a.question_id == q.id)
                    .map(|a| a.labels.iter().cloned().map(Value::String).collect())
                    .unwrap_or_default();
                by_id.insert(wire_id.clone(), json!({ "answers": labels }));
            }
            client.respond(&id, json!({ "answers": by_id }));
        });
        return;
    }
    let is_approval = matches!(
        method,
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
    );
    if !is_approval {
        tracing::debug!(
            target: "zeron_harness::codex",
            "unhandled server request: {method}"
        );
        client.respond_error(&id, -32601, &format!("unsupported method: {method}"));
        return;
    }
    if auto_approve {
        client.respond(&id, json!({ "decision": "accept" }));
        return;
    }

    let question = approval_question(method, params, &id, &permission_policy);
    let client = client.clone();
    let request_input = Arc::clone(request_input);
    tokio::spawn(async move {
        // The engine's input bridge owns the `InputRequested`/`InputResolved`
        // lifecycle (it mints the request id the resolver is parked under);
        // emitting our own copy here doubled the doc's input part with an id
        // `respond_input` could never match.
        //
        // A dropped sender (caller went away) degrades to a decline so the
        // agent is unblocked — never silently allowed.
        let answers = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            (request_input)(vec![question.clone()]),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
        let decision = answers
            .iter()
            .find(|answer| answer.question_id == question.id)
            .and_then(|answer| answer.labels.first())
            .map(String::as_str);
        client.respond(
            &id,
            json!({
                "decision": match decision {
                    Some("Allow once") => "accept",
                    Some("Allow for task") => "acceptForSession",
                    _ => "decline",
                }
            }),
        );
    });
}

/// Parse `item/tool/requestUserInput` questions into (wire id, question)
/// pairs, tolerant of field spellings; answers key by the WIRE id.
fn user_input_questions(params: &Value) -> Vec<(String, UserInputQuestion)> {
    params
        .get("questions")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(ix, q)| {
            let field = |keys: [&str; 3]| {
                keys.iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_str))
                    .unwrap_or("")
                    .to_owned()
            };
            let wire_id = {
                let id = field(["id", "questionId", "question_id"]);
                if id.is_empty() { format!("q{ix}") } else { id }
            };
            let question = UserInputQuestion {
                id: new_message_id(),
                header: {
                    let h = field(["header", "title", "label"]);
                    if h.is_empty() {
                        "Codex question".into()
                    } else {
                        h
                    }
                },
                question: field(["question", "prompt", "text"]),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| a.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|op| match op {
                        Value::String(s) => s.clone(),
                        other => other
                            .get("label")
                            .or_else(|| other.get("value"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    })
                    .collect(),
                multi_select: ["multiSelect", "multi_select"]
                    .iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_bool))
                    .unwrap_or(false),
                approval: None,
            };
            (wire_id, question)
        })
        .collect()
}

/// Synthesize the yes/no question an approval request surfaces to the user.
fn approval_question(
    method: &str,
    params: &Value,
    provider_request_id: &Value,
    policy: &zeron_proto::PermissionPolicy,
) -> UserInputQuestion {
    let (header, question) = if method.contains("commandExecution") {
        let command = match params.get("command") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        (
            "Approve command".to_owned(),
            if command.is_empty() {
                "Codex wants to run a command. Allow it?".to_owned()
            } else {
                format!("Codex wants to run `{command}`. Allow it?")
            },
        )
    } else {
        let paths: Vec<&str> = params
            .get("changes")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c.get("path").and_then(Value::as_str))
            .collect();
        (
            "Approve file change".to_owned(),
            if paths.is_empty() {
                "Codex wants to modify files. Allow it?".to_owned()
            } else {
                format!("Codex wants to modify {}. Allow it?", paths.join(", "))
            },
        )
    };
    let action_kind = if method.contains("commandExecution") {
        zeron_proto::ApprovalActionKind::Command
    } else {
        zeron_proto::ApprovalActionKind::FileChange
    };
    UserInputQuestion {
        id: new_message_id(),
        header,
        question: format!("{question} Target: {}.", policy.machine_name),
        options: vec![
            "Deny".into(),
            "Allow once".into(),
            "Allow for task".into(),
            "Cancel".into(),
        ],
        multi_select: false,
        approval: Some(zeron_proto::ApprovalRequest {
            provider_request_id: provider_request_id.to_string(),
            provider: HarnessId::Codex,
            turn_id: params
                .get("turnId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            machine_name: policy.machine_name.clone(),
            action_kind,
            summary: question,
            requested_scope: policy.scope,
            expires_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64
                + 120_000,
            supports_task_allowance: true,
        }),
    }
}

use crate::{Signal, send_signal, shutdown_child};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_steer_owner_clears_only_on_its_terminal_turn() {
        let mut owner = Some("turn-new".to_string());
        clear_explicit_steer_turn(&mut owner, "turn-stale");
        assert_eq!(owner.as_deref(), Some("turn-new"));
        clear_explicit_steer_turn(&mut owner, "turn-new");
        assert!(owner.is_none());
        clear_explicit_steer_turn(&mut owner, "turn-new");
        assert!(owner.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn named_command_passes_file_auth_override_to_synthetic_child() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("fake-codex");
        let capture = root.path().join("capture.txt");
        let home = root.path().join("named-home");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$CODEX_HOME\" \"$@\" > \"$ARGV_CAPTURE\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let mut command = named_command(&executable, "login", &home);
        command.env("ARGV_CAPTURE", &capture);
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        let lines = std::fs::read_to_string(capture).unwrap();
        assert_eq!(
            lines.lines().collect::<Vec<_>>(),
            vec![
                home.to_str().unwrap(),
                "-c",
                "cli_auth_credentials_store=\"file\"",
                "login"
            ]
        );
    }

    #[test]
    fn approval_questions_are_typed_and_machine_scoped() {
        let policy = zeron_proto::PermissionPolicy {
            machine_name: "test-machine".into(),
            ..Default::default()
        };
        let q = approval_question(
            "item/commandExecution/requestApproval",
            &json!({"itemId": "c1", "command": "rm -rf /tmp/x"}),
            &json!("provider-1"),
            &policy,
        );
        assert_eq!(q.header, "Approve command");
        assert!(q.question.contains("rm -rf /tmp/x"));
        assert_eq!(
            q.options,
            vec!["Deny", "Allow once", "Allow for task", "Cancel"]
        );
        assert!(!q.multi_select);
        assert_eq!(q.approval.unwrap().machine_name, "test-machine");

        let q = approval_question(
            "item/fileChange/requestApproval",
            &json!({"changes": [{"path": "/a.rs"}, {"path": "/b.rs"}]}),
            &json!("provider-2"),
            &policy,
        );
        assert_eq!(q.header, "Approve file change");
        assert!(q.question.contains("/a.rs, /b.rs"));

        // Command as argv array joins with spaces.
        let q = approval_question(
            "item/commandExecution/requestApproval",
            &json!({"command": ["git", "push", "--force"]}),
            &json!("provider-3"),
            &policy,
        );
        assert!(q.question.contains("git push --force"));
    }

    #[test]
    fn permission_modes_map_to_exact_app_server_settings() {
        use zeron_proto::PermissionMode::{Ask, Auto, Full};
        assert_eq!(
            codex_permission_settings(Ask, false),
            ("on-request", "user")
        );
        assert_eq!(
            codex_permission_settings(Auto, false),
            ("on-request", "auto_review")
        );
        assert_eq!(codex_permission_settings(Full, false), ("never", "user"));
        assert_eq!(
            sandbox_mode(zeron_proto::SandboxLevel::WorkspaceWrite),
            "workspace-write"
        );
        assert_eq!(
            sandbox_mode(zeron_proto::SandboxLevel::DangerFullAccess),
            "danger-full-access"
        );
    }

    #[test]
    fn model_page_skips_hidden_and_unknown_efforts() {
        let page = json!({
            "data": [
                {
                    "id": "hidden",
                    "model": "hidden",
                    "displayName": "Hidden",
                    "hidden": true,
                    "supportedReasoningEfforts": [{ "reasoningEffort": "high" }]
                },
                {
                    "id": "gpt-6-astra",
                    "model": "gpt-6-astra",
                    "displayName": "GPT-6-Astra",
                    "description": "  Most capable  ",
                    "hidden": false,
                    "supportedReasoningEfforts": [
                        { "reasoningEffort": "high" },
                        { "reasoningEffort": "future" }
                    ],
                    "serviceTiers": [{ "id": "priority", "name": "Fast" }],
                    "additionalSpeedTiers": ["fast"],
                    "defaultServiceTier": null,
                    "isDefault": true
                }
            ],
            "nextCursor": "next"
        });
        let (models, cursor) = parse_model_list_page(&page);
        assert_eq!(cursor.as_deref(), Some("next"));
        assert_eq!(models.len(), 1);
        let (astra, is_default) = &models[0];
        assert_eq!(astra.id, "gpt-6-astra");
        assert_eq!(astra.description.as_deref(), Some("Most capable"));
        assert_eq!(astra.reasoning_levels, vec![ReasoningLevel::High]);
        assert!(*is_default);
        assert_eq!(astra.options[0].choices.len(), 2);
        assert_eq!(astra.options[0].choices[1].id, "fast");
    }

    #[test]
    fn turn_router_never_revives_completed_turns() {
        let mut r = TurnRouter::default();
        r.note_completed("t-1");
        // The turn/start response arriving after turn/completed must not
        // resurrect the turn.
        r.adopt_started("t-1".into());
        assert_eq!(r.active, None);
        // Nor may a late turn/started notification.
        r.note_started("t-1".into());
        assert_eq!(r.active, None);

        r.note_started("t-2".into());
        assert_eq!(r.active.as_deref(), Some("t-2"));
        // A replacement started turn retires the stale one.
        r.note_started("t-3".into());
        assert_eq!(r.active.as_deref(), Some("t-3"));
        assert!(r.is_completed("t-2"));
    }
}
