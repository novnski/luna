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
//!   ordinary warm input (the turn-completed race) is queued as the next
//!   `turn/start` on the same thread. The session is persistent across turns
//!   while the steering mailbox lives. Explicit steer never starts a new turn.
//!   RPC acceptance is pending; native userMessage.clientId/readback records it.
//! - Interrupt: cancelling [`RunControls::interrupt`] sends `turn/interrupt`,
//!   escalating to SIGTERM → SIGKILL if the child is unresponsive; the stream
//!   always ends with `Done { status: Interrupted }`.

mod approvals;
pub(crate) mod catalog;
pub(crate) mod composer_catalog;
mod elicitation;
mod goals;
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
use tokio_util::sync::CancellationToken;

use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ModelOption, ModelOptionChoice,
    ProviderForkCapability, ProviderForkRequest, ProviderForkResult, ReasoningLevel, RunRequest,
    SlashCommand, SteeringMode, UserInputAnswer, UserInputQuestion,
};

use crate::jsonrpc::{Incoming, RpcClient, RpcRequestFailure};
use crate::process::{Child, Command, Stdio};
use crate::{
    Harness, HarnessError, LunaToolInvocation, LunaToolInvocationContext,
    ProviderForkProgressReporter, RunControls, SteerAcceptance,
};
use catalog::{REASONING_LEVELS, sandbox_mode, sandbox_policy_value, to_effort};
use normalize::{
    ChildRoute, ContextCompactionStream, Phase, ReasoningStream, delta_text, item_id, item_turn_id,
    item_type, notification_thread_id, route_child_notification, turn_error_message, turn_id,
    usage_event,
};

/// Engine-only model-option keys. They are consumed by this adapter and never
/// forwarded as model configuration.
pub const LUNA_NATIVE_THREAD_NAME_OPTION: &str = "lunaNativeThreadName";
pub const LUNA_STEER_RECONCILE_OPTION: &str = "lunaSteerReconcileIds";
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
    catalog_connections: composer_catalog::CatalogConnections,
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
            catalog_connections: composer_catalog::CatalogConnections::default(),
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
    /// `skills/list`. This separate process does not observe the selected
    /// session or prove effective skill attachment.
    async fn discover_skills_list(&self) -> Result<Value, HarnessError> {
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
                            "name": "luna-native",
                            "title": "Luna",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "capabilities": { "experimentalApi": true },
                    }),
                )
                .await?;
            client.notify("initialized", None);
            client.request("skills/list", json!({})).await
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }

    /// Picker behavior remains name-deduped and description-preserving.
    async fn discover_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let skills = self.discover_skills_list().await?;
        Ok(parse_skill_commands(&skills))
    }

    /// Internal read-only observation from a separate discovery process.
    /// Cwd/name pairs are not configured inventory or session attachment.
    #[doc(hidden)]
    pub async fn discover_skill_observations(
        &self,
    ) -> Result<CodexSkillDiscoveryObservations, HarnessError> {
        let skills = self.discover_skills_list().await?;
        Ok(observe_skill_discovery(&skills))
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
                            "name": "luna-native",
                            "title": "Luna",
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
                if !page.get("data").is_some_and(Value::is_array)
                    || page
                        .get("nextCursor")
                        .is_some_and(|cursor| !cursor.is_null() && !cursor.is_string())
                {
                    return Err(HarnessError::Protocol(
                        "model/list returned an invalid catalog page".into(),
                    ));
                }
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
                    return Err(HarnessError::Protocol(
                        "model/list returned a repeated pagination cursor".into(),
                    ));
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

/// Codex accepts both names, but Luna has historically persisted `fast`.
/// Normalize the app server's `priority` id to the historical persisted value.
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

const MAX_SKILL_OBSERVATION_GROUPS: usize = 64;
const MAX_SKILL_OBSERVATIONS_PER_GROUP: usize = 128;
const MAX_SKILL_OBSERVATIONS_TOTAL: usize = 512;
const MAX_SKILL_OBSERVATION_CWD_BYTES: usize = 1_024;
const MAX_SKILL_OBSERVATION_NAME_BYTES: usize = 128;

/// One sanitized provider-reported cwd/name pair from a separate discovery
/// process. This is neither an action target nor selected-session readback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct CodexSkillDiscoveryObservation {
    pub cwd: String,
    pub name: String,
}

/// An omitted flag makes bounds or malformed data visible to internal callers.
/// Even without omissions, this probe does not assert a complete inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct CodexSkillDiscoveryObservations {
    pub entries: Vec<CodexSkillDiscoveryObservation>,
    pub omitted: bool,
}

/// Keep the `skills/list` cwd grouping and duplicate names across roots. Only
/// bounded cwd/name strings survive; descriptions and other payload are dropped.
#[doc(hidden)]
pub fn observe_skill_discovery(result: &Value) -> CodexSkillDiscoveryObservations {
    let mut observed = CodexSkillDiscoveryObservations::default();
    let Some(groups) = result.get("data").and_then(Value::as_array) else {
        observed.omitted = true;
        return observed;
    };
    if groups.len() > MAX_SKILL_OBSERVATION_GROUPS {
        observed.omitted = true;
        return observed;
    }
    for group in groups {
        let Some(cwd) = group.get("cwd").and_then(Value::as_str).filter(|cwd| {
            !cwd.trim().is_empty()
                && cwd.len() <= MAX_SKILL_OBSERVATION_CWD_BYTES
                && !cwd.chars().any(char::is_control)
        }) else {
            observed.omitted = true;
            continue;
        };
        let Some(skills) = group.get("skills").and_then(Value::as_array) else {
            observed.omitted = true;
            continue;
        };
        if skills.len() > MAX_SKILL_OBSERVATIONS_PER_GROUP {
            observed.omitted = true;
            continue;
        }
        for skill in skills {
            let Some(name) = skill.get("name").and_then(Value::as_str).filter(|name| {
                !name.is_empty()
                    && name.trim() == *name
                    && name.len() <= MAX_SKILL_OBSERVATION_NAME_BYTES
                    && name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
                    })
            }) else {
                observed.omitted = true;
                continue;
            };
            if observed.entries.len() >= MAX_SKILL_OBSERVATIONS_TOTAL {
                observed.omitted = true;
                return observed;
            }
            observed.entries.push(CodexSkillDiscoveryObservation {
                cwd: cwd.to_owned(),
                name: name.to_owned(),
            });
        }
    }
    observed
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
    fn luna_runtime_delivery(&self) -> luna_proto::RuntimeDelivery {
        luna_proto::RuntimeDelivery::CodexDynamicTools
    }
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
        self.fork_session_staged(request, &|_| Ok(())).await
    }

    async fn fork_session_staged(
        &self,
        request: ProviderForkRequest,
        progress: &ProviderForkProgressReporter,
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
        client.fork_thread_staged(request, progress).await
    }

    /// The installed task CLI's `model/list` is the only runnable catalog.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.discover_models(None, false).await
    }

    async fn composer_catalog(
        &self,
        cwd: &std::path::Path,
        home: &std::path::Path,
        force_file_auth: bool,
        thread_id: Option<&str>,
    ) -> Result<luna_proto::ComposerCatalog, HarnessError> {
        let cwd = cwd.canonicalize().map_err(HarnessError::Io)?;
        if let Some(client) =
            thread_id.and_then(|thread| self.catalog_connections.get(home, &cwd, thread))
        {
            return tokio::time::timeout(
                Duration::from_secs(8),
                composer_catalog::read(&client, &cwd, thread_id),
            )
            .await
            .map_err(|_| HarnessError::Protocol("native thread catalog timed out".into()))?;
        }
        native::CodexNativeClient::new(home)
            .with_executable(self.resolve_executable()?)
            .for_execution_home(home, force_file_auth)
            .read_composer_catalog(&cwd, thread_id)
            .await
    }

    fn composer_catalog_source(&self) -> String {
        let Ok(path) = self.resolve_executable() else {
            return "unavailable".into();
        };
        let metadata = std::fs::metadata(&path).ok();
        format!(
            "{}:{:?}:{:?}",
            path.display(),
            metadata.as_ref().map(|m| m.len()),
            metadata.and_then(|m| m.modified().ok())
        )
    }

    /// Skills from a short-lived `skills/list` probe, cached on success.
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
        request.review = None;
        request.resume = None;
        request.worktree = None;
        request.attachments.clear();
        request.input_items.clear();
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
    /// Account-scoped discovery from the installed CLI for callers that
    /// persist a last-known-good catalog.
    pub async fn live_models_for_home(
        &self,
        codex_home: &std::path::Path,
        force_file_auth: bool,
    ) -> Result<Vec<Model>, HarnessError> {
        self.discover_models(Some(codex_home), force_file_auth)
            .await
    }

    async fn run_with_mode(
        &self,
        mut request: RunRequest,
        controls: RunControls,
        title_only: bool,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if !title_only
            && let Some(value) = request
                .model_options
                .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
        {
            let contract: luna_proto::LunaAgentRuntimeContract =
                serde_json::from_value(value.clone()).map_err(|error| {
                    HarnessError::Protocol(format!("invalid LUNA runtime contract: {error}"))
                })?;
            if contract.context.harness != HarnessId::Codex
                || !matches!(
                    contract.delivery,
                    luna_proto::RuntimeDelivery::CodexDynamicTools
                        | luna_proto::RuntimeDelivery::CodexInstructionsOnly
                )
                || (contract.delivery == luna_proto::RuntimeDelivery::CodexInstructionsOnly
                    && !contract.tools.is_empty())
                || (!contract.tools.is_empty() && controls.invoke_luna_tool.is_none())
            {
                return Err(HarnessError::Protocol(
                    "Codex LUNA tool contract has no matching engine executor".into(),
                ));
            }
        }
        crate::validate_composer_inputs(&request.attachments, &request.input_items)?;
        let exe = self.resolve_executable()?;
        // Title generation has no tools and remains read-only. Ordinary runs
        // keep the engine-resolved sandbox; never broaden it in the adapter.
        if title_only {
            request.sandbox = luna_proto::SandboxLevel::ReadOnly;
        }
        let mut cmd = Command::new(&exe);
        if let Some(contract) = request
            .model_options
            .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
            .and_then(|value| {
                serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone()).ok()
            })
        {
            for (name, value) in contract.environment {
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
        }
        let force_file_auth = request
            .model_options
            .remove(LUNA_CODEX_FILE_AUTH_OPTION)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if force_file_auth {
            apply_file_auth_override(&mut cmd);
        }
        cmd.arg("app-server");
        let catalog_home = request
            .model_options
            .remove(LUNA_CODEX_HOME_OPTION)
            .and_then(|value| value.as_str().map(std::path::PathBuf::from))
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .or_else(|| crate::executable::home_dir().map(|home| home.join(".codex")));
        if let Some(codex_home) = &catalog_home {
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
                    tracing::debug!(target: "luna_harness::codex", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (client, incoming) = RpcClient::new(stdin, stdout);
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        let (input_ready_tx, input_ready_rx) = tokio::sync::oneshot::channel();
        let validate_selections = !request.input_items.is_empty();
        tokio::spawn(run_session(Session {
            input_ready: validate_selections.then_some(input_ready_tx),
            title_only,
            catalog_home,
            catalog_connections: self.catalog_connections.clone(),
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

        if validate_selections {
            input_ready_rx.await.map_err(|_| HarnessError::RejectedBeforeAcceptance("Codex stopped before validating the selected inputs; your input remains available".into()))??;
        }
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
    input_ready: Option<tokio::sync::oneshot::Sender<Result<(), HarnessError>>>,
    title_only: bool,
    catalog_home: Option<PathBuf>,
    catalog_connections: composer_catalog::CatalogConnections,
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

/// Real Codex inline review (0.159): `review/start` returns the root turn,
/// one `turn/started` announces a review sub-turn with a different id, and
/// every item plus the final `turn/completed` carry the root id. The sub-turn
/// is an alias of the root: it must not retire the root or receive Stop.
#[derive(Default)]
struct ReviewTurn {
    root: Option<String>,
    aliases: HashSet<String>,
}

impl ReviewTurn {
    fn start(&mut self, root: String) {
        self.root = (!root.is_empty()).then_some(root);
        self.aliases.clear();
    }

    /// Canonical id for a `turn/started`: a different id announced while the
    /// review root is the active turn is the review sub-turn.
    fn started(&mut self, id: String, active: Option<&str>) -> String {
        match self.root.as_deref() {
            Some(root) if !id.is_empty() && id != root && active == Some(root) => {
                self.aliases.insert(id);
                root.to_owned()
            }
            _ => id,
        }
    }

    fn is_alias(&self, id: &str) -> bool {
        self.aliases.contains(id)
    }

    fn canonical(&self, id: String) -> String {
        match self.root.as_ref() {
            Some(root) if self.aliases.contains(&id) => root.clone(),
            _ => id,
        }
    }

    fn settle(&mut self, id: &str) {
        if self.root.as_deref() == Some(id) {
            self.root = None;
            self.aliases.clear();
        }
    }
}

/// A provider call ID may be delivered more than once. Keep one bounded set
/// for the active turn; an old turn cannot pass the router's active-turn check.
#[derive(Default)]
struct LunaCallIds {
    turn_id: Option<String>,
    seen: HashSet<String>,
    mcp_seen: HashSet<String>,
}

impl LunaCallIds {
    fn claim(&mut self, turn_id: &str, call_id: &str) -> bool {
        const MAX_CALL_ID_BYTES: usize = 256;
        const MAX_CALLS_PER_TURN: usize = 4_096;
        if call_id.is_empty() || call_id.len() > MAX_CALL_ID_BYTES {
            return false;
        }
        if self.turn_id.as_deref() != Some(turn_id) {
            self.turn_id = Some(turn_id.to_owned());
            self.seen.clear();
            self.mcp_seen.clear();
        }
        self.seen.len() < MAX_CALLS_PER_TURN && self.seen.insert(call_id.to_owned())
    }
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

/// IDs correlate delivery; they never authorize retry or deduplicate upstream.
#[derive(Default)]
struct NativeSteers {
    pending: HashMap<String, (Option<String>, Option<String>)>,
}

impl NativeSteers {
    fn record(&mut self, item: &Value, turn_id: &str) -> Option<Option<String>> {
        if !matches!(item_type(item), "userMessage" | "user_message") {
            return None;
        }
        let id = item.get("clientId").and_then(Value::as_str)?;
        let (expected, _) = self.pending.get(id)?;
        if expected
            .as_deref()
            .is_some_and(|expected| expected != turn_id)
        {
            return None;
        }
        self.pending.remove(id).map(|(_, message_id)| message_id)
    }
}

async fn emit_recorded_steer(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    assistant_message_id: &mut String,
    user_message_id: Option<String>,
    historical: bool,
) -> bool {
    let (previous, next) = if historical {
        (None, None)
    } else {
        let (prev, next) = rotate(assistant_message_id);
        (Some(prev), Some(next))
    };
    send(
        event_tx,
        AgentEvent::Steered {
            user_message_id,
            assistant_message_id: previous,
            next_assistant_message_id: next,
        },
    )
    .await
}

/// A missing history item proves nothing. Read once with a bounded wait and
/// retain every unresolved ID for late notifications or a later reconnect.
async fn reconcile_native_steers(
    client: &RpcClient,
    thread_id: &str,
    steers: &mut NativeSteers,
    assistant_message_id: &mut String,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    historical: bool,
) -> bool {
    if steers.pending.is_empty() {
        return true;
    }
    let read = tokio::time::timeout(
        Duration::from_secs(2),
        client.request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        ),
    )
    .await;
    let Ok(Ok(value)) = read else {
        return true;
    };
    let Some(thread) = value
        .get("thread")
        .filter(|thread| thread.get("id").and_then(Value::as_str) == Some(thread_id))
    else {
        return true;
    };
    for turn in thread
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(turn_id) = turn.get("id").and_then(Value::as_str) else {
            continue;
        };
        for item in turn
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(message_id) = steers.record(item, turn_id)
                && !emit_recorded_steer(event_tx, assistant_message_id, message_id, historical)
                    .await
            {
                return false;
            }
        }
    }
    true
}

async fn send(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

/// `turn/start` and return the new turn id from the response.
async fn start_turn(client: &RpcClient, params: Value) -> Result<String, HarnessError> {
    let started = client.request("turn/start", params).await?;
    Ok(started["turn"]["id"].as_str().unwrap_or("").to_owned())
}

/// Review owns the already-resolved thread. A separate LUNA review chat starts
/// a fresh native thread first; deprecated detached provider delivery is avoided.
async fn start_review(
    client: &RpcClient,
    thread_id: &str,
    review: &luna_proto::NativeReviewRequest,
) -> Result<String, HarnessError> {
    review.target.validate().map_err(HarnessError::Protocol)?;
    let started = client
        .request(
            "review/start",
            json!({
                "threadId": thread_id,
                "delivery": "inline",
                "target": review.target,
            }),
        )
        .await?;
    if started["reviewThreadId"].as_str() != Some(thread_id) {
        return Err(HarnessError::Protocol(
            "Codex returned an unexpected review thread; the task binding was preserved".into(),
        ));
    }
    let turn_id = started["turn"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| HarnessError::Protocol("Codex review/start returned no turn id".into()))?;
    Ok(turn_id.to_owned())
}

/// Awaiting a provider reply must not suspend Stop or consumer teardown.
/// None is an uncertain outcome: dispatch may already have reached Codex.
struct SessionLiveness<'a> {
    interrupt: &'a CancellationToken,
    events: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
}

impl SessionLiveness<'_> {
    async fn wait<T>(&self, operation: impl std::future::Future<Output = T>) -> Option<T> {
        tokio::select! {
            biased;
            _ = self.interrupt.cancelled() => None,
            _ = self.events.closed() => None,
            result = operation => Some(result),
        }
    }
}

/// The per-run event loop: one task multiplexing app-server messages, the
/// steering mailbox, the interrupt token, and consumer liveness.
async fn run_session(session: Session) {
    let Session {
        mut input_ready,
        title_only,
        catalog_home,
        catalog_connections,
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
        invoke_luna_tool,
    } = controls;
    let liveness = SessionLiveness {
        interrupt: &interrupt,
        events: &event_tx,
    };
    let request_input = Arc::new(request_input);
    let runtime_contract = request
        .model_options
        .get(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
        .and_then(|value| {
            serde_json::from_value::<luna_proto::LunaAgentRuntimeContract>(value.clone()).ok()
        })
        .filter(|contract| contract.context.harness == HarnessId::Codex);

    // ---- wire params ------------------------------------------------------
    let effective_mode = request
        .permission_policy
        .as_ref()
        .map(|policy| policy.effective_mode)
        .unwrap_or(luna_proto::PermissionMode::Ask);
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
            if let Some(contract) = runtime_contract.as_ref() {
                if !contract.instructions.is_empty() {
                    p.insert(
                        "developerInstructions".into(),
                        render_luna_instructions(contract).into(),
                    );
                }
                if contract.delivery == luna_proto::RuntimeDelivery::CodexDynamicTools
                    && invoke_luna_tool.is_some()
                {
                    p.insert(
                        "dynamicTools".into(),
                        Value::Array(
                            contract
                                .tools
                                .iter()
                                .map(|tool| {
                                    json!({
                                        "type": "function",
                                        "name": tool.name,
                                        "description": tool.description,
                                        "inputSchema": tool.input_schema,
                                    })
                                })
                                .collect(),
                        ),
                    );
                }
            }
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
        if request.review.is_some()
            && let Some(effort) = effort
        {
            // review/start has no per-turn effort field; freeze the selected
            // reasoning setting into thread/start or thread/resume instead.
            p.entry("config".to_string())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .expect("thread config is an object")
                .insert("model_reasoning_effort".into(), effort.into());
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
                        "name": "luna-native",
                        "title": "Luna",
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
            // Dynamic tools are retained in the provider's original thread
            // metadata; thread/resume has no replacement dynamicTools field.
            p.remove("dynamicTools");
            p.insert("threadId".into(), Value::String(resume.clone()));
            match client.request("thread/resume", Value::Object(p)).await {
                Ok(thread) => thread,
                // A missing/foreign rollout falls back to a fresh thread.
                Err(e) if !strict_resume => {
                    tracing::debug!(
                        target: "luna_harness::codex",
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
        if request.review.is_some() && thread_id.is_empty() {
            return Err(HarnessError::Protocol(
                "Codex returned no thread identity for native review".into(),
            ));
        }
        // Native thread/start and thread/resume report this policy beside `thread`.
        // thread/read does not expose it; keep the provider-reported loaded state.
        client.set_composer_policy(
            &thread_id,
            thread
                .get("disabledPluginIds")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
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
            tracing::warn!(target: "luna_harness::codex", %thread_id, %error, "native thread name set failed");
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
        _ = event_tx.closed() => {
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };

    // Metadata requests reuse the exact loaded native runtime. The lease drops
    // with this session and never keeps a child alive after adapter shutdown.
    let catalog_cwd = std::path::Path::new(&request.cwd)
        .canonicalize()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| request.cwd.clone());
    let _catalog_lease = catalog_home
        .as_deref()
        .map(|home| catalog_connections.register(home, &catalog_cwd, &thread_id, client.clone()));

    // Retained for the entire warm subscription; generation-aware Drop cannot
    // unregister a replacement session. Never register ephemeral title work.
    let _goal_connection = if title_only {
        None
    } else {
        catalog_home
            .as_ref()
            .map(|home| goals::register(home, &thread_id, &client))
    };
    if !title_only
        && let Some(Ok(Ok(reply))) = liveness
            .wait(tokio::time::timeout(
                Duration::from_secs(5),
                client.request("thread/goal/get", json!({"threadId": thread_id})),
            ))
            .await
        && let Ok(goal) = goals::decode(&thread_id, &reply)
    {
        let _ = send(
            &event_tx,
            AgentEvent::GoalChanged {
                thread_id: thread_id.clone(),
                goal,
            },
        )
        .await;
    }

    let turn_params = |input: Vec<Value>| -> Value {
        let mut p = serde_json::Map::new();
        p.insert("threadId".into(), Value::String(thread_id.clone()));
        p.insert("input".into(), Value::Array(input));
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

    let initial_input = match crate::composer_input::codex_input(
        &request.prompt,
        &request.attachments,
        &request.input_items,
    ) {
        Ok(input) => input,
        Err(error) => {
            if let Some(tx) = input_ready.take() {
                let _ = tx.send(Err(error));
            } else {
                let _ = event_tx.send(Err(error)).await;
            }
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };
    let Some(validation) = liveness
        .wait(composer_catalog::validate_inputs(
            &client,
            std::path::Path::new(&request.cwd),
            &thread_id,
            &request.input_items,
        ))
        .await
    else {
        shutdown_child(&mut child, kill_grace).await;
        return;
    };
    if let Err(error) = validation {
        if let Some(tx) = input_ready.take() {
            let _ = tx.send(Err(error));
        } else {
            let _ = event_tx.send(Err(error)).await;
        }
        shutdown_child(&mut child, kill_grace).await;
        return;
    }
    if let Some(tx) = input_ready.take() {
        let _ = tx.send(Ok(()));
    }

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

    let mut native_steers = NativeSteers::default();
    if request.resume.as_deref() == Some(thread_id.as_str()) {
        for id in request
            .model_options
            .get(LUNA_STEER_RECONCILE_OPTION)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            native_steers
                .pending
                .insert(id.to_string(), (None, Some(id.to_string())));
        }
        if !reconcile_native_steers(
            &client,
            &thread_id,
            &mut native_steers,
            &mut assistant_message_id,
            &event_tx,
            true,
        )
        .await
        {
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    }

    let mut router = TurnRouter::default();
    let mut review_turn = ReviewTurn::default();
    let mut luna_call_ids = LunaCallIds::default();
    let initial_turn = async {
        match request.review.as_ref() {
            Some(review) => start_review(&client, &thread_id, review).await,
            None => start_turn(&client, turn_params(initial_input)).await,
        }
    };
    let Some(started) = liveness.wait(initial_turn).await else {
        if !event_tx.is_closed() {
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
        }
        shutdown_child(&mut child, kill_grace).await;
        return;
    };
    match started {
        Ok(id) => {
            if request.review.is_some() {
                review_turn.start(id.clone());
            }
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
    let mut review_output_pending = request.review.is_some();
    // Codex repeats the exitedReviewMode text as a final agent message; the
    // findings part already carries it.
    let mut review_output: Option<String> = None;
    let mut streamed_text: HashSet<String> = HashSet::new();
    let mut async_questions_seen: HashSet<String> = HashSet::new();
    let mut reasoning_streams: HashMap<String, ReasoningStream> = HashMap::new();
    let mut context_compactions = ContextCompactionStream::default();
    let mut computer_use = normalize::ComputerUseStream::for_task(
        runtime_contract
            .as_ref()
            .map(|contract| contract.context.thread_id.as_str()),
    );
    let mut computer_use_turn_cancel = CancellationToken::new();
    let mut pending_approvals = approvals::PendingApprovals::new();
    // Token usage is held until the turn ends, emitted just before Done.
    let mut pending_usage: Option<AgentEvent> = None;
    // Steers whose `turn/steer` lost the turn-completed race; delivered as the
    // next `turn/start` when the expected turn's end notification arrives.
    // Typed input, its composer selections and the durable correlation ID.
    let mut queued_steers: VecDeque<(Vec<Value>, Vec<luna_proto::ComposerInput>, Option<String>)> =
        VecDeque::new();
    // Acknowledged input remains pending until its exact clientId is recorded.
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    // A Done has been emitted for the turn currently/last in flight.
    let mut done_current = false;
    let mut done_after_interrupt = false;
    let mut terminal_error = None;
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
                    "thread/goal/updated" | "thread/goal/cleared" => {
                        if notification_thread_id(&method, &params).as_deref() != Some(thread_id.as_str()) { continue; }
                        let goal = if method == "thread/goal/cleared" { None } else {
                            match goals::decode(&thread_id, &params) {
                                Ok(goal) => goal,
                                Err(error) => { tracing::warn!(%error, "invalid goal notification"); continue; }
                            }
                        };
                        if !send(&event_tx, AgentEvent::GoalChanged { thread_id: thread_id.clone(), goal }).await { break 'main; }
                    }
                    "serverRequest/resolved" => {
                        if params.get("threadId").and_then(Value::as_str) == Some(thread_id.as_str())
                            && let Some(id) = params.get("requestId")
                            && let Some(cancel) = pending_approvals.remove(&id.to_string())
                        {
                            cancel.cancel();
                        }
                    }
                    "turn/started" => {
                        let id = review_turn.started(turn_id(&params), router.active_id());
                        let previous = router.active_id().map(str::to_owned);
                        router.note_started(id.clone());
                        // A late start for a completed turn must not rotate
                        // the live turn's approval token or settle its calls.
                        if router.active_id() != Some(id.as_str()) {
                            continue;
                        }
                        if previous.as_deref() != Some(id.as_str()) {
                            done_current = false;
                            if !send(&event_tx, AgentEvent::ProviderTurnStarted { thread_id: thread_id.clone(), turn_id: id.clone() }).await { break 'main; }
                        }
                        if computer_use_turn_cancel.is_cancelled() || previous.as_deref() != Some(id.as_str()) {
                            computer_use_turn_cancel.cancel();
                            computer_use_turn_cancel = CancellationToken::new();
                        }
                        for event in computer_use.unknown_before_turn(&thread_id, &id) {
                            if !send(&event_tx, event).await { break 'main; }
                        }
                        if let Some(event) = context_compactions
                            .unknown_before_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
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
                        if phase == Phase::Completed && matches!(item_type(item), "exitedReviewMode" | "exited_review_mode") {
                            review_output_pending = false;
                            review_output = item
                                .get("review")
                                .and_then(Value::as_str)
                                .map(|text| text.trim().to_owned())
                                .filter(|text| !text.is_empty());
                        }
                        if request.review.is_some()
                            && matches!(item_type(item), "agentMessage" | "agent_message")
                            && let Some(review) = review_output.as_deref()
                            && item.get("text").and_then(Value::as_str).map(str::trim) == Some(review)
                        {
                            continue;
                        }
                        // Item notifications carry the root `turnId`, not `turn.id`.
                        let item_turn = item_turn_id(&params);
                        if let Some(message_id) = native_steers.record(item, &item_turn) {
                            if !emit_recorded_steer(&event_tx, &mut assistant_message_id, message_id,
                                router.active_id() != Some(item_turn.as_str())).await {
                                break 'main;
                            }
                            continue;
                        }

                        if interrupt.is_cancelled() {
                            // An OS action may already be running when Stop
                            // arrives. A late item completion cannot turn its
                            // uncertain outcome into a successful chip.
                            for event in computer_use.unknown_for_turn(&thread_id, router.active_id().unwrap_or("")) {
                                if !send(&event_tx, event).await { break 'main; }
                            }
                            if normalize::is_computer_use(item) {
                                continue;
                            }
                        }
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
                        if let Some(events) = computer_use.map_item(phase, &thread_id, router.active_id(), &params, item) {
                            for event in events {
                                if !send(&event_tx, event).await { break 'main; }
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
                                // `request_user_input_async` is delivered as
                                // assistant-message metadata, not as a
                                // `tool/requestUserInput` server request.
                                // Preserve the typed questions alongside the
                                // ordinary text so the UI can offer a later
                                // answer without parking this turn.
                                if let Some(AgentEvent::AsyncQuestions { message_id, questions }) =
                                    async_questions_from_item(item)
                                    && async_questions_seen.insert(message_id.clone())
                                    && !send(&event_tx, AgentEvent::AsyncQuestions {
                                        message_id,
                                        questions,
                                    }).await
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

                    "item/commandExecution/outputDelta" | "item/commandExecution/terminalInteraction"
                    | "item/fileChange/outputDelta" | "item/fileChange/patchUpdated" => {
                        for event in normalize::tool_activity(&method, &params) {
                            if !send(&event_tx, event).await { break 'main; }
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
                        let raw = turn_id(&params);
                        // A review sub-turn completion ends the review only
                        // once reviewer output arrived; the root completes it.
                        if review_turn.is_alias(&raw) && review_output_pending {
                            router.note_completed(&raw);
                            continue;
                        }
                        let id = review_turn.canonical(raw);
                        if router.active_id() != Some(id.as_str()) {
                            router.note_completed(&id);
                            continue;
                        }
                        review_turn.settle(&id);
                        computer_use_turn_cancel.cancel();
                        for event in computer_use.unknown_for_turn(&thread_id, &id) {
                            if !send(&event_tx, event).await { break 'main; }
                        }
                        if let Some(event) = context_compactions
                            .unknown_for_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
                        if !reconcile_native_steers(&client, &thread_id, &mut native_steers,
                            &mut assistant_message_id, &event_tx, false).await { break 'main; }
                        router.note_completed(&id);
                        // Item ids never span turns; without this the set grew
                        // one entry per message for a persistent session's life.
                        streamed_text.clear();
                        async_questions_seen.clear();
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
                        // Native review: a turn that ends without reviewer output is
                        // an error even when Codex reports it completed.
                        let review_missing = !interrupted && review_output_pending;
                        let error = error.or_else(|| {
                            review_missing
                                .then(|| "Native review ended without reviewer output".to_string())
                        });
                        review_output_pending = false;
                        review_output = None;
                        let status = match params.pointer("/turn/status").and_then(Value::as_str) {
                            Some("interrupted") => DoneStatus::Interrupted,
                            Some("failed") => DoneStatus::Errored,
                            Some("completed") if review_missing => DoneStatus::Errored,
                            Some("completed") => DoneStatus::Completed,
                            _ if interrupted => DoneStatus::Interrupted,
                            _ if error.is_some() => DoneStatus::Errored,
                            _ => DoneStatus::Completed,
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
                        if interrupted || status == DoneStatus::Interrupted {
                            interrupted = true;
                            done_after_interrupt = true;
                            break 'main;
                        }
                        // Persistent session: a steer that lost the race with
                        // this turn's end becomes the next turn now; otherwise
                        // stay alive for the mailbox — the caller owns teardown.
                        if let Some((input, selections, message_id)) = queued_steers.pop_front() {
                            if !steer_as_new_turn(
                                &client,
                                turn_params(input),
                                message_id,
                                &request.cwd, &thread_id, &selections,
                                &mut router,
                                &liveness,
                                &mut assistant_message_id,
                                &mut done_current,
                                &mut computer_use_turn_cancel,
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
                        let id = review_turn.canonical(turn_id(&params));
                        if router.active_id() != Some(id.as_str()) {
                            router.note_completed(&id);
                            continue;
                        }
                        review_turn.settle(&id);
                        computer_use_turn_cancel.cancel();
                        for event in computer_use.unknown_for_turn(&thread_id, &id) {
                            if !send(&event_tx, event).await { break 'main; }
                        }
                        if let Some(event) = context_compactions
                            .unknown_for_turn(&thread_id, &id)
                            && !send(&event_tx, event).await
                        {
                            break 'main;
                        }
                        if !reconcile_native_steers(&client, &thread_id, &mut native_steers,
                            &mut assistant_message_id, &event_tx, false).await { break 'main; }
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
                        let id = review_turn.canonical(turn_id(&params));
                        if router.active_id() != Some(id.as_str()) {
                            router.note_completed(&id);
                            continue;
                        }
                        review_turn.settle(&id);
                        computer_use_turn_cancel.cancel();
                        for event in computer_use.unknown_for_turn(&thread_id, &id) {
                            if !send(&event_tx, event).await { break 'main; }
                        }
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
                    pending_approvals.retain(|_, token| !token.is_cancelled());
                    handle_server_request(
                        &client,
                        id,
                        &method,
                        &params,
                        ServerRequestContext {
                            auto_approve: request.auto_approve,
                            permission_policy: request.permission_policy.clone().unwrap_or_default(),
                            request_input: &request_input,
                            thread_id: &thread_id,
                            active_turn_id: router.active_id(),
                            runtime_contract: runtime_contract.as_ref(),
                            invoke_luna_tool: invoke_luna_tool.as_ref(),
                            call_ids: &mut luna_call_ids,
                            interrupt: &interrupt,
                            computer_use_turn_cancel: &computer_use_turn_cancel,
                            pending_approvals: &mut pending_approvals,
                        },
                    );
                }

                // stdout EOF or reader gone: the app server exited.
                Some(Incoming::Eof) | None => break 'main,
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    let Some(validation) = liveness.wait(composer_catalog::validate_inputs(&client, std::path::Path::new(&request.cwd), &thread_id, &msg.input_items)).await else { break 'main; };
                    if let Err(error) = validation {
                        if let Some(tx) = msg.acceptance { let _ = tx.send(SteerAcceptance::Rejected(error.to_string())); }
                        else { terminal_error = Some(error.to_string()); break 'main; }
                        continue 'main;
                    }
                    let selections = msg.input_items.clone();
                    let input = match crate::composer_input::codex_input(&msg.prompt, &msg.attachments, &msg.input_items) {
                        Ok(input) => input,
                        Err(error) => {
                            if let Some(tx) = msg.acceptance { let _ = tx.send(SteerAcceptance::Rejected(error.to_string())); }
                            else { terminal_error = Some(error.to_string()); break 'main; }
                            continue 'main;
                        }
                    };
                    let require_active =
                        msg.delivery == crate::SteerDelivery::RequireActiveTurn;
                    let mut acceptance = msg.acceptance;
                    let message_id = msg.message_id;
                    let wire_id = message_id.clone().unwrap_or_else(new_message_id);
                    if let Some(expected) = router.active.clone() {
                        let steer_params = json!({
                            "threadId": thread_id,
                            "expectedTurnId": expected,
                            "clientUserMessageId": wire_id,
                            "input": input,
                        });
                        native_steers.pending.insert(wire_id.clone(),
                            (Some(expected.clone()), message_id.clone()));
                        let Some(reply) = liveness
                            .wait(async {
                                tokio::time::timeout(Duration::from_secs(5),
                                    client.request_classified("turn/steer", steer_params)).await
                                    .unwrap_or_else(|_| Err(RpcRequestFailure::Unknown(
                                        "Codex steer acknowledgement timed out".into())))
                            })
                            .await
                        else {
                            if let Some(tx) = acceptance.take() {
                                let _ = tx.send(SteerAcceptance::Unknown(
                                    "Codex session stopped before the steer reply arrived".into(),
                                ));
                            }
                            break 'main;
                        };
                        match reply {
                            Ok(reply) => {
                                // RPC success acknowledges scheduling only. A clientId
                                // notification/readback creates the transcript boundary.
                                if let Some(tx) = acceptance.take() {
                                    let outcome = if reply.get("turnId").and_then(Value::as_str)
                                        == Some(expected.as_str()) {
                                        SteerAcceptance::Accepted
                                    } else {
                                        SteerAcceptance::Unknown("Codex steer returned a different or missing turn id".into())
                                    };
                                    let _ = tx.send(outcome);
                                }
                            }
                            // Only a correlated rejection authorizes ordinary
                            // warm input to fall back to the next turn. A lost
                            // reply may already have delivered the input.
                            Err(failure) => {
                                if matches!(&failure, RpcRequestFailure::Rejected(_)) {
                                    native_steers.pending.remove(&wire_id);
                                }
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
                                if let RpcRequestFailure::Unknown(message) = failure {
                                    if let Some(tx) = acceptance.take() {
                                        let _ = tx.send(SteerAcceptance::Unknown(message.clone()));
                                    }
                                    terminal_error = Some(format!(
                                        "Codex steering delivery is unknown; input was not replayed: {message}"
                                    ));
                                    // Notifications may already contain the exact native
                                    // record. Keep consuming; never start a fallback turn.
                                    continue 'main;
                                }
                                tracing::debug!(
                                    target: "luna_harness::codex",
                                    "turn/steer did not settle (queued as next turn): {}",
                                    failure.message()
                                );
                                if router.active.as_deref() == Some(expected.as_str())
                                    && !router.is_completed(&expected)
                                {
                                    queued_steers.push_back((input, selections, message_id));
                                } else if !steer_as_new_turn(
                                    &client,
                                    turn_params(input),
                                    message_id,
                                    &request.cwd, &thread_id, &selections,
                                    &mut router,
                                    &liveness,
                                    &mut assistant_message_id,
                                    &mut done_current,
                                    &mut computer_use_turn_cancel,
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
                        turn_params(input),
                        message_id,
                        &request.cwd, &thread_id, &selections,
                        &mut router,
                        &liveness,
                        &mut assistant_message_id,
                        &mut done_current,
                        &mut computer_use_turn_cancel,
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
                computer_use_turn_cancel.cancel();
                if let Some(turn) = router.active.clone() {
                    for event in computer_use.unknown_for_turn(&thread_id, &turn) {
                        if !send(&event_tx, event).await { break 'main; }
                    }
                    let client = client.clone();
                    let thread = thread_id.clone();
                    tokio::spawn(async move {
                        if let Err(e) = client
                            .request("turn/interrupt", json!({ "threadId": thread, "turnId": turn }))
                            .await
                        {
                            tracing::debug!(
                                target: "luna_harness::codex",
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
    interrupted |= interrupt.is_cancelled();
    computer_use_turn_cancel.cancel();
    if !event_tx.is_closed() {
        for event in computer_use.take_unknown() {
            let _ = event_tx.send(Ok(event)).await;
        }
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
                    error: Some(terminal_error.unwrap_or_else(|| {
                        crate::crash_message("codex app-server", status, &stderr_tail)
                    })),
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

/// The only source of nonblocking question cards is Codex's typed async
/// agent-message item. Ordinary prose and the blocking server request never
/// enter this path.
fn async_questions_from_item(item: &Value) -> Option<AgentEvent> {
    if item.get("delivery")?.as_str()? != "async" {
        return None;
    }
    let message_id = item.get("id")?.as_str()?.to_string();
    if message_id.is_empty() {
        return None;
    }
    let questions: Vec<luna_proto::AsyncUserInputQuestion> =
        serde_json::from_value(item.get("questions")?.clone()).ok()?;
    if questions.is_empty() || questions.iter().any(|q| q.title.trim().is_empty()) {
        return None;
    }
    Some(AgentEvent::AsyncQuestions {
        message_id,
        questions,
    })
}

fn codex_permission_settings(
    mode: luna_proto::PermissionMode,
    title_only: bool,
) -> (&'static str, &'static str) {
    if title_only {
        return ("never", "user");
    }
    match mode {
        luna_proto::PermissionMode::Ask => ("on-request", "user"),
        luna_proto::PermissionMode::Auto => ("on-request", "auto_review"),
        luna_proto::PermissionMode::Full => ("never", "user"),
    }
}

/// Deliver a steer as a fresh `turn/start` on the same thread (the fallback
/// leg of the steer race, and the between-turns delivery path). Returns false
/// when the loop should end (turn/start failed or the consumer hung up).
#[allow(clippy::too_many_arguments)]
async fn steer_as_new_turn(
    client: &RpcClient,
    mut params: Value,
    user_message_id: Option<String>,
    cwd: &str,
    thread_id: &str,
    selections: &[luna_proto::ComposerInput],
    router: &mut TurnRouter,
    liveness: &SessionLiveness<'_>,
    assistant_message_id: &mut String,
    done_current: &mut bool,
    computer_use_turn_cancel: &mut CancellationToken,
) -> bool {
    if let Some(id) = &user_message_id {
        params["clientUserMessageId"] = json!(id);
    }
    let Some(started) = liveness
        .wait(async {
            let attachments: Vec<String> = params
                .get("input")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("localImage"))
                .filter_map(|item| item.get("path").and_then(Value::as_str).map(str::to_owned))
                .collect();
            crate::validate_composer_inputs(&attachments, selections)?;
            composer_catalog::validate_inputs(
                client,
                std::path::Path::new(cwd),
                thread_id,
                selections,
            )
            .await?;
            start_turn(client, params).await
        })
        .await
    else {
        return false;
    };
    let event_tx = liveness.events;
    match started {
        Ok(id) => {
            router.adopt_started(id.clone());
            if router.active_id() == Some(id.as_str()) {
                computer_use_turn_cancel.cancel();
                *computer_use_turn_cancel = CancellationToken::new();
            }
            *done_current = false;
            let (prev, next) = rotate(assistant_message_id);
            if !send(
                event_tx,
                AgentEvent::Steered {
                    user_message_id,
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
// Approvals (approval-as-input parity with luna's UX)
// ---------------------------------------------------------------------------

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

fn render_luna_instructions(contract: &luna_proto::LunaAgentRuntimeContract) -> String {
    let mut text = String::new();
    for section in &contract.instructions {
        text.push_str(&format!(
            "[LUNA {:?}: {} | {}]\n{}\n\n",
            section.source, section.name, section.provenance, section.text
        ));
    }
    text
}

/// Immutable authority and routing context for one provider request.
struct ServerRequestContext<'a> {
    auto_approve: bool,
    permission_policy: luna_proto::PermissionPolicy,
    request_input: &'a Arc<RequestInputFn>,
    thread_id: &'a str,
    active_turn_id: Option<&'a str>,
    runtime_contract: Option<&'a luna_proto::LunaAgentRuntimeContract>,
    invoke_luna_tool: Option<&'a crate::LunaToolExecutor>,
    call_ids: &'a mut LunaCallIds,
    interrupt: &'a CancellationToken,
    computer_use_turn_cancel: &'a CancellationToken,
    pending_approvals: &'a mut approvals::PendingApprovals,
}

/// Serve one server→client request. Approval requests round-trip through the
/// input bridge; dynamic LUNA tools go to the engine's scoped registry.
fn handle_server_request(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
    context: ServerRequestContext<'_>,
) {
    if matches!(
        method,
        "item/permissions/requestApproval"
            | "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
    ) {
        let mut pending = std::mem::take(context.pending_approvals);
        approvals::handle(client, id, method, params, &context, &mut pending);
        *context.pending_approvals = pending;
        return;
    }
    let ServerRequestContext {
        auto_approve: _,
        permission_policy,
        request_input,
        thread_id,
        active_turn_id,
        runtime_contract,
        invoke_luna_tool,
        call_ids,
        interrupt,
        computer_use_turn_cancel,
        pending_approvals: _,
    } = context;
    if method == "mcpServer/elicitation/request"
        && params.get("serverName").and_then(Value::as_str) != Some("computer-use")
    {
        let exact_context = !interrupt.is_cancelled()
            && !computer_use_turn_cancel.is_cancelled()
            && active_turn_id.is_some()
            && params.get("threadId").and_then(Value::as_str) == Some(thread_id)
            && params.get("turnId").and_then(Value::as_str) == active_turn_id
            && runtime_contract.is_some_and(|contract| {
                !contract.context.thread_id.is_empty() && !contract.context.machine_id.is_empty()
            });
        if !exact_context {
            client.respond(&id, json!({ "action": "decline" }));
            return;
        }
        let turn = active_turn_id.unwrap();
        if call_ids.turn_id.as_deref() != Some(turn) {
            call_ids.turn_id = Some(turn.to_owned());
            call_ids.seen.clear();
            call_ids.mcp_seen.clear();
        }
        let wire_id = id.to_string();
        // A retransmitted opaque request ID shares the original resolver.
        // Do not send a competing decline response to the same pending ID.
        if call_ids.mcp_seen.contains(&wire_id) {
            return;
        }
        if call_ids.mcp_seen.len() >= 32 {
            client.respond(&id, json!({ "action": "decline" }));
            return;
        }
        let Ok(questions) = elicitation::questions(params, &id) else {
            client.respond(&id, json!({ "action": "decline" }));
            return;
        };
        call_ids.mcp_seen.insert(wire_id);
        let client = client.clone();
        let request_input = Arc::clone(request_input);
        let interrupt = interrupt.clone();
        let turn_cancel = computer_use_turn_cancel.clone();
        tokio::spawn(async move {
            let receiver = (request_input)(questions.clone());
            let answers = tokio::select! {
                biased;
                _ = interrupt.cancelled() => None,
                _ = turn_cancel.cancelled() => None,
                result = tokio::time::timeout(Duration::from_secs(120), receiver) => result.ok().and_then(Result::ok),
            };
            let response = if interrupt.is_cancelled() || turn_cancel.is_cancelled() {
                json!({ "action": "cancel" })
            } else {
                answers
                    .as_ref()
                    .and_then(|answers| luna_proto::mcp_response(&questions, answers).ok())
                    .unwrap_or_else(|| json!({ "action": "cancel" }))
            };
            client.respond(&id, response);
        });
        return;
    }
    if method == "mcpServer/elicitation/request" {
        let matching_context = !interrupt.is_cancelled()
            && !computer_use_turn_cancel.is_cancelled()
            && active_turn_id.is_some()
            && params.get("serverName").and_then(Value::as_str) == Some("computer-use")
            && params.get("threadId").and_then(Value::as_str) == Some(thread_id)
            && params.get("turnId").and_then(Value::as_str) == active_turn_id
            && runtime_contract.is_some_and(|contract| {
                !contract.context.thread_id.is_empty() && !contract.context.machine_id.is_empty()
            });
        let Some((field, once)) = matching_context
            .then(|| computer_use_once_choice(params))
            .flatten()
        else {
            client.respond(&id, json!({ "action": "decline" }));
            return;
        };
        let Some(app) = computer_use_approval_app(params) else {
            client.respond(&id, json!({ "action": "decline" }));
            return;
        };
        let task_id = runtime_contract
            .map(|contract| contract.context.thread_id.as_str())
            .unwrap_or("");
        let question = UserInputQuestion {
            id: new_message_id(),
            header: "Approve computer use".into(),
            question: format!(
                "Allow Codex in LUNA task {task_id} to use {app} on {} for this request? The provider controls the app and LUNA cannot undo an action already running.",
                permission_policy.machine_name
            ),
            options: vec!["Deny".into(), "Allow once".into()],
            multi_select: false,
            mcp: None,
            approval: Some(luna_proto::ApprovalRequest {
                provider_request_id: id.to_string(),
                provider: HarnessId::Codex,
                turn_id: active_turn_id.map(str::to_owned),
                machine_name: permission_policy.machine_name.clone(),
                action_kind: luna_proto::ApprovalActionKind::Tool,
                summary: format!("Computer use: {app}"),
                requested_scope: permission_policy.scope,
                expires_at_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64
                    + 120_000,
                supports_task_allowance: false,
                details: None,
            }),
        };
        let client = client.clone();
        let request_input = Arc::clone(request_input);
        let interrupt = interrupt.clone();
        let turn_cancel = computer_use_turn_cancel.clone();
        tokio::spawn(async move {
            let receiver = (request_input)(vec![question.clone()]);
            let answers = tokio::select! {
                biased;
                _ = interrupt.cancelled() => None,
                _ = turn_cancel.cancelled() => None,
                result = tokio::time::timeout(Duration::from_secs(120), receiver) => result.ok().and_then(Result::ok),
            };
            let allowed = !interrupt.is_cancelled()
                && !turn_cancel.is_cancelled()
                && answers.as_ref().is_some_and(|answers| {
                    answers.iter().any(|answer| {
                        answer.question_id == question.id
                            && answer.labels.as_slice() == ["Allow once"]
                    })
                });
            if allowed {
                let mut content = serde_json::Map::new();
                content.insert(field, once.into());
                client.respond(&id, json!({ "action": "accept", "content": content }));
            } else {
                client.respond(&id, json!({ "action": "decline" }));
            }
        });
        return;
    }
    if method == "item/tool/call" {
        let call_id = params.get("callId").and_then(Value::as_str).unwrap_or("");
        let matching_context = active_turn_id.is_some()
            && params.get("threadId").and_then(Value::as_str) == Some(thread_id)
            && params.get("turnId").and_then(Value::as_str) == active_turn_id
            && params.get("namespace").is_none_or(Value::is_null)
            && !interrupt.is_cancelled();
        let name = params.get("tool").and_then(Value::as_str).unwrap_or("");
        let attached = runtime_contract
            .is_some_and(|contract| contract.tools.iter().any(|tool| tool.name == name));
        let Some(executor) = invoke_luna_tool.filter(|_| matching_context && attached) else {
            client.respond(&id, json!({
                "success": false,
                "contentItems": [{"type":"inputText","text":"LUNA tool is not attached to this task or turn"}]
            }));
            return;
        };
        let Some(turn_id) = active_turn_id.filter(|turn_id| call_ids.claim(turn_id, call_id))
        else {
            client.respond(&id, json!({
                "success": false,
                "contentItems": [{"type":"inputText","text":"LUNA tool call ID is missing, repeated, or over limit"}]
            }));
            return;
        };
        let client = client.clone();
        let executor = Arc::clone(executor);
        let interrupt = interrupt.clone();
        let invocation = LunaToolInvocation {
            name: name.to_owned(),
            arguments: params.get("arguments").cloned().unwrap_or(Value::Null),
            context: LunaToolInvocationContext::new(thread_id, turn_id, call_id),
        };
        tokio::spawn(async move {
            let outcome = tokio::select! {
                biased;
                _ = interrupt.cancelled() => Err(HarnessError::Protocol("LUNA tool call cancelled".into())),
                result = executor(invocation) => result,
            };
            let (success, text) = match outcome {
                Ok(value) => (true, value.to_string()),
                Err(error) => (false, error.to_string()),
            };
            client.respond(
                &id,
                json!({
                    "success": success,
                    "contentItems": [{"type":"inputText","text":text}]
                }),
            );
        });
        return;
    }
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
    tracing::debug!(target: "luna_harness::codex", "unhandled server request: {method}");
    client.respond_error(&id, -32601, &format!("unsupported method: {method}"));
}

/// Only a form choice explicitly advertising a one-request approval can be
/// sent back to Codex. Session/permanent approvals stay unavailable in L28.
fn computer_use_once_choice(params: &Value) -> Option<(String, String)> {
    if params.get("mode").and_then(Value::as_str) != Some("form") {
        return None;
    }
    let properties = params.pointer("/requestedSchema/properties")?.as_object()?;
    for (field, schema) in properties {
        if params
            .pointer("/requestedSchema/required")
            .and_then(Value::as_array)
            .is_some_and(|required| {
                required
                    .iter()
                    .any(|value| value.as_str() != Some(field.as_str()))
            })
        {
            continue;
        }
        let choices = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("const").and_then(Value::as_str))
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                schema
                    .get("enum")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        if let Some(choice) = choices
            .into_iter()
            .find(|value| matches!(*value, "once" | "allow_once" | "accept_once"))
        {
            return Some((field.clone(), choice.to_owned()));
        }
    }
    None
}

fn computer_use_approval_app(params: &Value) -> Option<String> {
    let name = params
        .pointer("/_meta/app_name")
        .or_else(|| params.pointer("/_meta/appName"))
        .or_else(|| params.pointer("/_meta/target/app"))
        .or_else(|| params.pointer("/_meta/target/name"))
        .or_else(|| params.pointer("/_meta/tool_params/app_name"))
        .or_else(|| params.pointer("/_meta/tool_params/app"))
        .and_then(Value::as_str)
        .or_else(|| {
            params
                .get("message")
                .and_then(Value::as_str)
                .and_then(|message| {
                    message
                        .strip_prefix("Allow ChatGPT to use ")?
                        .strip_suffix('?')
                })
        })?;
    let name = name.trim();
    (!name.is_empty() && name.len() <= 256).then(|| name.to_owned())
}

#[cfg(test)]
mod computer_use_approval_tests {
    use super::*;

    #[test]
    fn only_explicit_one_request_form_choice_is_supported() {
        let form = json!({"mode":"form","requestedSchema":{"properties":{"approval":{"oneOf":[{"const":"once"},{"const":"session"}]}}}});
        assert_eq!(
            computer_use_once_choice(&form),
            Some(("approval".into(), "once".into()))
        );
        assert_eq!(
            computer_use_once_choice(
                &json!({"mode":"url","requestedSchema":form["requestedSchema"]})
            ),
            None
        );
        assert_eq!(
            computer_use_once_choice(
                &json!({"mode":"form","requestedSchema":{"properties":{"approval":{"enum":["always","session"]}}}})
            ),
            None
        );
    }

    #[test]
    fn app_target_requires_a_concrete_provider_name() {
        assert_eq!(
            computer_use_approval_app(&json!({"_meta":{"app_name":"Finder"}})),
            Some("Finder".into())
        );
        assert_eq!(
            computer_use_approval_app(&json!({"message":"Allow ChatGPT to use Safari?"})),
            Some("Safari".into())
        );
        assert_eq!(
            computer_use_approval_app(&json!({"message":"Allow this tool?"})),
            None
        );
    }
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
                mcp: None,
                approval: None,
            };
            (wire_id, question)
        })
        .collect()
}

/// Synthesize the yes/no question an approval request surfaces to the user.
#[cfg(test)]
fn approval_question(
    method: &str,
    params: &Value,
    provider_request_id: &Value,
    policy: &luna_proto::PermissionPolicy,
) -> UserInputQuestion {
    approvals::legacy_question(method, params, provider_request_id, policy)
}

use crate::{Signal, send_signal, shutdown_child};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn async_question_requires_typed_agent_message_metadata() {
        let item = json!({
            "type": "agentMessage",
            "id": "call-42",
            "text": "Which sample should I use?",
            "delivery": "async",
            "questions": [{"title": "Which sample should I use?", "options": ["A", "B"]}]
        });
        assert_eq!(
            async_questions_from_item(&item),
            Some(AgentEvent::AsyncQuestions {
                message_id: "call-42".into(),
                questions: vec![luna_proto::AsyncUserInputQuestion {
                    title: "Which sample should I use?".into(),
                    options: Some(vec!["A".into(), "B".into()]),
                }],
            })
        );
        let mut ordinary = item.clone();
        ordinary.as_object_mut().unwrap().remove("delivery");
        assert!(async_questions_from_item(&ordinary).is_none());
        ordinary["delivery"] = json!("async");
        ordinary["questions"] = json!([]);
        assert!(async_questions_from_item(&ordinary).is_none());
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
        let policy = luna_proto::PermissionPolicy {
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
            vec!["Deny", "Allow once", "Allow for session", "Cancel turn"]
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
        use luna_proto::PermissionMode::{Ask, Auto, Full};
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
            sandbox_mode(luna_proto::SandboxLevel::WorkspaceWrite),
            "workspace-write"
        );
        assert_eq!(
            sandbox_mode(luna_proto::SandboxLevel::DangerFullAccess),
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
