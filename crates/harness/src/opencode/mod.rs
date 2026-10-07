//! Native OpenCode driver over OpenCode 2's own HTTP + SSE API — the
//! surface the OpenCode desktop app and the official `Standalone` launcher
//! use. OpenCode 1.x (1.18) is no longer supported (PD-15): the resolver
//! skips 1.x binaries and a 1.x-only machine gets an actionable error.
//!
//! Transport decision (OC-02 step 7): HTTP + SSE is kept. ACP (`opencode
//! acp`, a lossy private translation of the same HTTP surface that runs one
//! prompt at a time), the TypeScript SDK/client (a JS sidecar with no wire
//! gain) and `opencode run --format json` (a one-shot projection without
//! approvals or forms) are rejected.
//!
//! The wire, verified against 2.0.22:
//! - spawn `opencode serve --stdio --port 0` with `OPENCODE_PASSWORD`
//!   (HTTP Basic, username `opencode`): stdin is the ownership lease, the
//!   stdout `{"url": ...}` line is readiness, and `GET /api/info` proves a
//!   2.x server. The boot checks the live `/openapi.json` against the
//!   operations and request fields LUNA depends on (`compat`).
//! - one global SSE bus (`GET /api/event`) carries every session's traffic,
//!   child (subagent) sessions included, token-level. Frames are rewritten
//!   into internal 1.x-shaped payloads ([`normalize_v2_frame`]) so the turn
//!   engine stays as it was; unknown event types are logged once.
//! - a turn is `POST /api/session/{id}/prompt` (model set once on the
//!   session); the END of the turn is the terminal execution frame for
//!   THAT session (`session.execution.succeeded/.failed/.interrupted`).
//! - reasoning streams as reasoning items → [`AgentEvent::ReasoningDelta`].
//!
//! A prompt that produces NO session-scoped event within the stall bound
//! (`LUNA_OPENCODE_STALL_MS`, 0 disables) errors out instead of spinning
//! "Working" forever. `LUNA_OPENCODE_WIRE_LOG=<dir>` appends a redacted
//! JSONL wire log for QA (`wire_log`).
//!
//! Module map (each submodule does `use super::*`, and this module
//! glob-imports each submodule, so names resolve crate-locally):
//! - this file: [`OpencodeHarness`] and its `Harness` impl.
//! - `resolve`: executable resolution (2.x only, version-probed).
//! - `server`: the `opencode serve` child, readiness, HTTP helpers.
//! - `compat`: the boot-time OpenAPI contract and the pinned event set.
//! - `wire_log`: the opt-in redacted QA wire log.
//! - `catalog`: models, variants, slash commands, session model switch.
//! - `usage`: usage, cost, context occupancy, model readback, compaction.
//! - `inputs`: images as `files[]`, typed skills, input admission.
//! - `history`: message listing, fork planning and readback.
//! - `turn`: the run loop (setup, first prompt, steering, settlement).
//! - `transport`: the SSE bus reader; `wire_v2`: the 2.x frame normalizer.
//! - `bus`: routing one normalized payload; `parts`: part streaming state.
//! - `subagents`: spawn chips and child binding; `tools`: tool vocabulary.
//! - `permissions`: rules, approval options, permission replies.
//! - `forms`: v2 forms (questions); `prompt`: session create, turn submit.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ProviderForkBoundary, ProviderForkCapability,
    ProviderForkRequest, ProviderForkResult, ReasoningLevel, RunRequest, SlashCommand,
    SteeringMode, TodoItem, ToolCall, UserInputAnswer, UserInputQuestion,
};

use crate::process::{Child, ChildStdin, Command, Stdio};
use crate::{
    CommandScope, ContextCompactRequest, Harness, HarnessError, ProviderForkProgress,
    ProviderForkProgressReporter, RunControls, shutdown_child,
};

mod bus;
mod catalog;
mod compat;
mod forms;
mod history;
mod inputs;
mod parts;
mod permissions;
mod prompt;
mod resolve;
mod server;
mod subagents;
mod tools;
mod transport;
mod turn;
mod usage;
mod wire_log;
mod wire_v2;

use bus::*;
use catalog::*;
use compat::*;
use inputs::*;
use parts::*;
use permissions::*;
use prompt::*;
use resolve::*;
use server::*;
use subagents::*;
use tools::*;
use transport::*;
use turn::*;
use usage::*;
use wire_log::*;
use wire_v2::*;

pub use compat::{CompatReport, CompatStatus, opencode_compat_report};
pub use inputs::{model_accepts_images, validate_inputs};
pub use resolve::{
    OpencodeResolution, OpencodeSource, last_opencode_resolution, resolve_opencode_executable,
};

/// Effort ladder surfaced in the picker; applied per run by picking the
/// first of these variant ids the chosen model actually advertises. Must
/// mirror the registry descriptor.
const REASONING_LEVELS: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

/// The location a discovery call answers for: the run's directory when one
/// is known, else the user's home (where global provider config lives).
fn discovery_location(cwd: Option<&str>) -> String {
    match cwd.filter(|cwd| !cwd.is_empty()) {
        Some(cwd) => cwd.to_owned(),
        None => crate::executable::home_or_current_dir()
            .to_string_lossy()
            .into_owned(),
    }
}

pub struct OpencodeHarness {
    executable: Option<PathBuf>,
    /// Test seam: an already-running server (no spawn, no auth unless given).
    base_url: Option<String>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    startup_timeout: Duration,
    models_cache: tokio::sync::Mutex<Option<(Instant, Vec<Model>)>>,
    /// Slash commands per discovery location (a run's cwd, or home).
    commands_cache: std::sync::Mutex<HashMap<String, Vec<SlashCommand>>>,
    /// Coalesce concurrent picker/title probes: several cold opencode boots
    /// at once are slower than one.
    probe_lock: tokio::sync::Mutex<()>,
    /// Live runs' servers by session: out-of-turn controls (compaction) go
    /// to the chat's own server instead of booting a second one.
    live: LiveServers,
    /// Bound on a compaction after its admission.
    compaction_wait: Duration,
}

impl Default for OpencodeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            base_url: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            startup_timeout: startup_timeout(),
            models_cache: tokio::sync::Mutex::new(None),
            commands_cache: std::sync::Mutex::new(HashMap::new()),
            probe_lock: tokio::sync::Mutex::new(()),
            live: LiveServers::default(),
            compaction_wait: compaction_wait(),
        }
    }
}

impl OpencodeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a fixed binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Drive an already-running server (tests): no spawn, no basic auth.
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = Some(base.into());
        self
    }

    pub fn with_graces(mut self, interrupt: Duration, kill: Duration) -> Self {
        self.interrupt_grace = interrupt;
        self.kill_grace = kill;
        self
    }

    /// The OpenCode 2 binary this harness launches: the explicit path (its
    /// existence validated like Codex and Claude overrides), else
    /// `OPENCODE_EXECUTABLE`, else the search. A 1.x-only machine gets
    /// "OpenCode 2 is required (found 1.18.x …)".
    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(exe) = &self.executable {
            let exe = crate::executable::validate_native_override(exe)?;
            return check_explicit(exe, OpencodeSource::Explicit).map(|r| r.path);
        }
        resolve_opencode().map(|resolution| resolution.path)
    }

    /// Boot (or attach to) a server for a run/probe in `cwd`.
    async fn server(&self, cwd: Option<&str>) -> Result<Server, HarnessError> {
        if let Some(base) = &self.base_url {
            return Ok(Server::attached(base.clone()));
        }
        let exe = self.resolve_executable()?;
        Server::spawn(&exe, cwd, self.startup_timeout).await
    }

    fn cached_commands(&self, location: &str) -> Option<Vec<SlashCommand>> {
        self.commands_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(location).cloned())
    }

    fn cache_commands(&self, location: &str, commands: Vec<SlashCommand>) {
        if let Ok(mut cache) = self.commands_cache.lock() {
            cache.insert(location.to_owned(), commands);
        }
    }

    /// One short-lived server, booted in the user's home and scoped to it
    /// (`x-opencode-directory`), answers both discovery calls. Also primes
    /// that location's commands so concurrent picker/composer fetches share
    /// one boot.
    async fn probe_models(&self) -> Result<Vec<Model>, HarnessError> {
        let _guard = self.probe_lock.lock().await;
        let location = discovery_location(None);
        let mut server = self.server(Some(&location)).await?;
        let result = async {
            let providers = server.provider_catalog(Some(&location)).await?;
            let models = models_from_providers(&providers);
            if models.is_empty() {
                return Err(HarnessError::Protocol(
                    "opencode advertised no models (`opencode auth login` to configure a provider)"
                        .into(),
                ));
            }
            if let Ok(commands) = server.commands_wire(Some(&location)).await {
                self.cache_commands(&location, commands_from_wire(&commands));
            }
            Ok(models)
        }
        .await;
        server.shutdown(self.kill_grace).await;
        result
    }

    /// Slash commands as the server sees them in `location`.
    async fn probe_commands(&self, location: &str) -> Result<Vec<SlashCommand>, HarnessError> {
        let _guard = self.probe_lock.lock().await;
        if let Some(commands) = self.cached_commands(location) {
            return Ok(commands);
        }
        let mut server = self.server(Some(location)).await?;
        let result = server
            .commands_wire(Some(location))
            .await
            .map(|v| commands_from_wire(&v));
        server.shutdown(self.kill_grace).await;
        if let Ok(commands) = &result {
            self.cache_commands(location, commands.clone());
        }
        result
    }
}

#[async_trait]
impl Harness for OpencodeHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Opencode
    }
    fn display_name(&self) -> &str {
        // Must match the registry's lazy descriptor.
        "OpenCode"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    /// Steers queue and deliver as the next prompt when the live turn goes
    /// idle (native mid-turn steering is not advertised yet, PD-2).
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        REASONING_LEVELS
    }
    /// Any OpenCode binary counts, whatever its generation: a 1.x-only
    /// machine keeps the provider visible, and its actions fail with the
    /// actionable "OpenCode 2 is required" error instead of it vanishing.
    fn installed(&self) -> bool {
        self.executable.is_some() || self.base_url.is_some() || opencode_present()
    }
    /// The execution terminal frames are real per-turn terminals: the
    /// engine can retire its quiesce watchdogs.
    fn deterministic_turn_end(&self) -> bool {
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
        let mut server = self.server(Some(&request.cwd)).await?;
        let result = server
            .fork_session_staged(
                &request.source_session_id,
                Some(&request.cwd),
                &request.boundary,
                progress,
            )
            .await;
        server.shutdown(self.kill_grace).await;
        result
    }

    /// Live discovery off `GET /api/model`. Only overlapping calls share a
    /// result, so provider/auth changes are visible on the next request.
    /// Failures remain retryable.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        if self.base_url.is_none() {
            self.resolve_executable()?;
        }
        let requested_at = Instant::now();
        let mut latest = self.models_cache.lock().await;
        if let Some((completed_at, models)) = &*latest
            && *completed_at >= requested_at
        {
            return Ok(models.clone());
        }
        let models = self.probe_models().await?;
        *latest = Some((Instant::now(), models.clone()));
        Ok(models)
    }

    /// The engine cache's refresh: the same live `GET /api/model` read
    /// (OpenCode has no bundled fallback list), so a failed probe reaches
    /// the cache, which keeps its last good snapshot, and an empty catalog
    /// is never saved (OCF-100).
    async fn models_live(&self) -> Result<Vec<Model>, HarnessError> {
        self.models().await
    }

    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let location = discovery_location(None);
        match self.cached_commands(&location) {
            Some(commands) => Ok(commands),
            None => self.probe_commands(&location).await,
        }
    }

    /// Commands as a run in `scope.cwd` would see them (project commands
    /// live in the location's config).
    async fn commands_in(&self, scope: &CommandScope) -> Result<Vec<SlashCommand>, HarnessError> {
        let cwd = scope
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned());
        let location = discovery_location(cwd.as_deref());
        match self.cached_commands(&location) {
            Some(commands) => Ok(commands),
            None => self.probe_commands(&location).await,
        }
    }

    /// Provider-owned compaction, complete only on `compaction.ended`. A
    /// chat with a live run compacts on that run's server (the item steers
    /// in at its next step boundary); otherwise a server is booted for the
    /// session and kept alive until the outcome. The boot is outside the
    /// post-admission bound.
    async fn compact_context(&self, request: ContextCompactRequest) -> Result<(), HarnessError> {
        let dir = (!request.cwd.is_empty()).then_some(request.cwd.as_str());
        if let Some(live) = self.live.get(&request.session_id) {
            return live
                .compact_session(&request.session_id, dir, self.compaction_wait)
                .await;
        }
        let mut server = self.server(dir).await?;
        let result = server
            .compact_session(&request.session_id, dir, self.compaction_wait)
            .await;
        server.shutdown(self.kill_grace).await;
        result
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // Images and typed selections are checked before anything boots:
        // a rejection keeps the draft (RejectedBeforeAcceptance).
        validate_inputs(
            request.model.as_deref(),
            &request.attachments,
            &request.input_items,
        )?;
        let cwd = (!request.cwd.is_empty()).then(|| request.cwd.clone());
        let server = self.server(cwd.as_deref()).await?;
        // Only that location's commands: a home-scoped list would miss the
        // project's own commands (the run fetches them when uncached).
        let known_commands = cwd.as_deref().and_then(|cwd| self.cached_commands(cwd));
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            server,
            event_tx,
            controls,
            request,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            known_commands,
            live: self.live.clone(),
        }));
        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod context_tests {
    use super::*;
    #[test]
    fn context_includes_cache_and_output_but_not_reasoning() {
        let tokens = json!({
            "input": 200, "output": 100, "reasoning": 50, "cache": {"read":40000,"write":1800}
        });
        assert_eq!(step_context_tokens(&tokens), 42100);
        assert_eq!(step_context_tokens(&json!({"input":0,"output":0})), 0);
    }
}
