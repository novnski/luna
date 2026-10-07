//! Native opencode driver — the same HTTP + SSE protocol the opencode
//! desktop app speaks, replacing the ACP path (`opencode acp`).
//!
//! Why not ACP: opencode's own ACP layer settles a prompt on the FIRST
//! `session.status{idle}` it observes after subscribing, uncorrelated with
//! the submitted turn, and it ignores `session.error` and `session.idle`
//! entirely (`packages/opencode/src/acp/event.ts`, 1.18.x) — the source of
//! "done while still working" and silently-hung turns. It also drops all
//! subagent traffic and thinking never reaches the ACP wire usefully. The
//! desktop app doesn't use ACP; neither do we.
//!
//! Two server generations are spoken, detected at boot from the health
//! endpoints ([`Protocol`]): the 1.18 "v1" wire (verified against 1.18.31)
//! and the 2.x `/api/*` wire (verified against 2.0.3):
//! - spawn `opencode serve --port <free> --hostname 127.0.0.1` with
//!   `OPENCODE_SERVER_PASSWORD=<uuid>` (HTTP Basic, username `opencode`);
//!   readiness + protocol = `GET /api/health` vs `GET /global/health`.
//! - one global SSE bus (`GET /api/event` on 2.x, `GET /global/event` on
//!   1.x) carries every session's traffic, child (subagent) sessions
//!   included, token-level. 2.x frames are rewritten into the 1.x payload
//!   shapes ([`normalize_v2_frame`]) so the turn engine is wire-agnostic.
//! - a turn is `POST /api/session/{id}/prompt` (2.x; model set once on the
//!   session) or `POST /session/{id}/prompt_async` (1.x, fire-and-forget
//!   204); the END of the turn is the terminal execution/status frame for
//!   THAT session — exactly what the desktop's working-predicate keys on.
//!   Busy is re-asserted at the top of every agent-loop iteration, so idle
//!   after busy is authoritative, not a lull.
//! - reasoning streams as reasoning parts on both wires →
//!   [`AgentEvent::ReasoningDelta`], the thinking feed.
//!
//! Failure surfacing (the #169 class): a dying provider is VISIBLE here —
//! `session.status{type:"retry", attempt, message}` streams per attempt.
//! Attempt ≥ [`RETRY_REPORT_ATTEMPT`] surfaces an error chip; attempt ≥
//! [`RETRY_ABORT_ATTEMPT`] aborts the turn instead of retrying forever.
//! A prompt that produces NO session-scoped event within
//! [`default_stall_bound`] (`LUNA_OPENCODE_STALL_MS`, 0 disables) errors
//! out instead of spinning "Working" forever.
//!
//! Module map (each submodule does `use super::*`, and this module
//! glob-imports each submodule, so names resolve crate-locally):
//! - this file: [`OpencodeHarness`] and its `Harness` impl.
//! - `resolve`: executable resolution.
//! - `server`: the `opencode serve` child, readiness, HTTP helpers.
//! - `catalog`: models, variants, slash commands, session model switch.
//! - `usage`: token/context usage and compaction.
//! - `history`: message listing, fork planning and readback.
//! - `turn`: the run loop (setup, first prompt, steering, settlement).
//! - `transport`: the SSE bus reader; `wire_v2`: the 2.x frame normalizer.
//! - `bus`: routing one normalized payload; `parts`: part streaming state.
//! - `subagents`: spawn chips and child binding; `tools`: tool vocabulary.
//! - `permissions`: rules, approval options, permission replies.
//! - `forms`: questions; `prompt`: session create and turn submission.

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

use crate::process::{Child, Command, Stdio};
use crate::{
    ContextCompactRequest, Harness, HarnessError, ProviderForkProgress,
    ProviderForkProgressReporter, RunControls, shutdown_child,
};

mod bus;
mod catalog;
mod forms;
mod history;
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
mod wire_v2;

use bus::*;
use catalog::*;
use forms::*;
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
use wire_v2::*;

pub use resolve::resolve_opencode_executable;

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

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

pub struct OpencodeHarness {
    executable: Option<PathBuf>,
    /// Test seam: an already-running server (no spawn, no auth unless given).
    base_url: Option<String>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    startup_timeout: Duration,
    models_cache: tokio::sync::Mutex<Option<(Instant, Vec<Model>)>>,
    commands_cache: tokio::sync::OnceCell<Vec<SlashCommand>>,
    /// Coalesce concurrent picker/title probes: several cold opencode boots
    /// at once are slower than one.
    probe_lock: tokio::sync::Mutex<()>,
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
            commands_cache: tokio::sync::OnceCell::new(),
            probe_lock: tokio::sync::Mutex::new(()),
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

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(exe) = &self.executable {
            return Ok(exe.clone());
        }
        resolve_opencode_executable().ok_or_else(|| HarnessError::NotInstalled(INSTALL_HINT.into()))
    }

    /// Boot (or attach to) a server for a run/probe. Probes have no chat cwd:
    /// they boot in the user's home, where global provider config lives.
    async fn server(&self, cwd: Option<&str>) -> Result<Server, HarnessError> {
        if let Some(base) = &self.base_url {
            return Ok(Server::attached(base.clone()));
        }
        let exe = self.resolve_executable()?;
        Server::spawn(&exe, cwd, self.startup_timeout).await
    }

    /// One short-lived server answers both discovery calls. Also primes the
    /// commands cache so concurrent picker/composer fetches share one boot.
    async fn probe_models(&self) -> Result<Vec<Model>, HarnessError> {
        let _guard = self.probe_lock.lock().await;
        let mut server = self.server(None).await?;
        let result = async {
            let providers = server.provider_catalog(None).await?;
            let models = models_from_providers(&providers);
            if models.is_empty() {
                return Err(HarnessError::Protocol(
                    "opencode advertised no models (`opencode auth login` to configure a provider)"
                        .into(),
                ));
            }
            if let Ok(commands) = server.commands_wire(None).await {
                let _ = self.commands_cache.set(commands_from_wire(&commands));
            }
            Ok(models)
        }
        .await;
        server.shutdown(self.kill_grace).await;
        result
    }

    async fn probe_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let _guard = self.probe_lock.lock().await;
        if let Some(commands) = self.commands_cache.get() {
            return Ok(commands.clone());
        }
        let mut server = self.server(None).await?;
        let result = server
            .commands_wire(None)
            .await
            .map(|v| commands_from_wire(&v));
        server.shutdown(self.kill_grace).await;
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
    /// idle — opencode has no mid-turn injection on this wire.
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        REASONING_LEVELS
    }
    fn installed(&self) -> bool {
        self.executable.is_some()
            || self.base_url.is_some()
            || resolve_opencode_executable().is_some()
    }
    /// `session.status{idle}` is a real terminal frame per turn: the engine
    /// can retire its quiesce watchdogs.
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

    /// Live discovery off `GET /provider` (what the desktop app populates its
    /// picker from). Only overlapping calls share a result, so provider/auth
    /// changes are visible on the next request. Failures remain retryable.
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

    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        self.commands_cache
            .get_or_try_init(|| self.probe_commands())
            .await
            .cloned()
    }

    async fn compact_context(&self, request: ContextCompactRequest) -> Result<(), HarnessError> {
        let mut server = self.server(Some(&request.cwd)).await?;
        let result = server
            .compact_session(
                &request.session_id,
                Some(&request.cwd),
                request.model.as_deref(),
            )
            .await;
        server.shutdown(self.kill_grace).await;
        result
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let cwd = (!request.cwd.is_empty()).then(|| request.cwd.clone());
        let server = self.server(cwd.as_deref()).await?;
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            server,
            event_tx,
            controls,
            request,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            known_commands: self.commands_cache.get().cloned(),
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
    fn context_includes_cache_and_uses_reported_model_limit() {
        let windows = HashMap::from([("provider/model".into(), 200000)]);
        let info = json!({"providerID":"provider","modelID":"model", "tokens": {
            "input": 200, "output": 100, "reasoning": 50, "cache": {"read":40000,"write":1800}
        }});
        assert_eq!(
            context_usage_event(&info, &windows),
            Some(AgentEvent::ContextUsage {
                tokens: Some(42100),
                window: Some(200000)
            })
        );
        assert_eq!(
            context_usage_event(&json!({"tokens":{"input":0,"output":0}}), &windows),
            None
        );
        assert_eq!(
            context_usage_event(
                &json!({"time":{"completed":1},"tokens":{"input":0,"output":0}}),
                &windows
            ),
            Some(AgentEvent::ContextUsage {
                tokens: Some(0),
                window: None
            })
        );
    }
}
