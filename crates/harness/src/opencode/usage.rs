//! OC-10: usage, cost, context occupancy, effective-model readback and
//! provider-owned compaction — the same shared LUNA mechanisms Codex and
//! Claude feed (`ContextUsage`, `ProviderAccounting`, `ContextCompaction`).
//!
//! Wire facts (OpenCode 2.0.22):
//! - `session.step.ended{cost, tokens}` is ONE provider request. Its
//!   `input + cache.read + cache.write + output` is what the context holds
//!   after the step (OpenCode's own `estimatePrompt` anchor), so it drives the
//!   context ring, with the step model's `limit.context` as the window.
//! - `session.usage.updated{cost, tokens}` is the session's RUNNING total
//!   (`projector.ts` re-reads the summed session row): cost and accounting
//!   only, never occupancy, and never a synthetic message.
//! - `session.step.started{model}` and `session.model.selected` name the
//!   model that actually runs; `GET /api/session/{id}` reads the session's
//!   current `model {providerID, id, variant}` back after `POST …/model`.
//! - `POST …/compact {id}` only ADMITS a compaction (an inbox item that
//!   steers in at the next step boundary and wakes an idle session). The
//!   outcome is `session.compaction.started{inputID}` followed by
//!   `session.compaction.ended` (no inputID; one compaction at a time per
//!   session) or `session.compaction.failed{inputID}`.

use luna_proto::{
    ContextOperationStatus, ModelAccounting, ProviderAccountingReport, ResultAccounting,
    RuntimeReadback, RuntimeReadbackSource, UsageProvider, UsageTokens,
};

use super::*;

/// Default bound on a compaction AFTER its admission (the cold boot of a
/// discovery server is not counted). Summarizing a long session is one model
/// call, which can take minutes on a slow provider.
pub(super) const DEFAULT_COMPACTION_WAIT: Duration = Duration::from_secs(600);
pub(super) const COMPACTION_WAIT_ENV: &str = "LUNA_OPENCODE_COMPACTION_TIMEOUT_SECS";

pub(super) fn compaction_wait() -> Duration {
    std::env::var(COMPACTION_WAIT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_COMPACTION_WAIT)
}

/// A caller-chosen message id in OpenCode's ascending format (`msg_` + 12
/// hex digits of `ms << 12 | counter` + 14 base62 characters), so inbox
/// items LUNA creates sort like the server's own.
pub(super) fn ascending_message_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    // Monotonic even within one millisecond (and across clock steps back).
    let mut current = now << 12;
    loop {
        let last = LAST.load(Ordering::Relaxed);
        let next = current.max(last + 1);
        if LAST
            .compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            current = next;
            break;
        }
    }
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let random = uuid::Uuid::new_v4();
    let suffix: String = random.as_bytes()[..14]
        .iter()
        .map(|byte| CHARS[(*byte as usize) % CHARS.len()] as char)
        .collect();
    format!("msg_{:012x}{suffix}", current & 0xffff_ffff_ffff)
}

/// `session.compact` takes `{id?, delivery?}`; LUNA names the inbox item
/// so the outcome can be correlated (`compaction.started.inputID`).
pub(super) fn compact_http_request(session_id: &str, input_id: &str) -> (String, Value) {
    (
        format!("/api/session/{session_id}/compact"),
        json!({ "id": input_id }),
    )
}

impl Server {
    /// Provider-owned compaction, reported complete only on
    /// `session.compaction.ended` (OCF-113). A live subscription exists
    /// before the admission (the bus has no replay); the wait is bounded
    /// after admission. `compaction.failed` is a definite "not applied"
    /// ([`HarnessError::RejectedBeforeAcceptance`]): history is unchanged.
    pub(super) async fn compact_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
        wait: Duration,
    ) -> Result<(), HarnessError> {
        let (tx, mut rx) = mpsc::channel::<BusMsg>(256);
        let bus = tokio::spawn(bus_task(self.base.clone(), self.auth.clone(), tx));
        let result = async {
            let connected = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    match rx.recv().await {
                        Some(BusMsg::Connected) => return true,
                        Some(BusMsg::Disconnected) | None => return false,
                        Some(_) => {}
                    }
                }
            })
            .await;
            if !matches!(connected, Ok(true)) {
                return Err(HarnessError::RejectedBeforeAcceptance(
                    "OpenCode's event stream did not connect, so the compaction was not started"
                        .into(),
                ));
            }
            let input_id = ascending_message_id();
            let (path, body) = compact_http_request(session_id, &input_id);
            let (status, text) = self.post_json_raw(&path, directory, &body).await?;
            if !status.is_success() {
                // A tagged 4xx (conflict, not found) means nothing was admitted.
                let detail = post_error_message(&path, status, &text);
                return Err(if status.is_client_error() {
                    HarnessError::RejectedBeforeAcceptance(detail)
                } else {
                    HarnessError::Protocol(detail)
                });
            }
            let admitted: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let inbox_id = admitted
                .pointer("/data/id")
                .and_then(Value::as_str)
                .unwrap_or(&input_id)
                .to_owned();
            tracing::info!(
                target: "luna_harness::opencode",
                "compaction admitted for {session_id} as {inbox_id}; waiting for compaction.ended"
            );
            let deadline = tokio::time::Instant::now() + wait;
            let mut started = false;
            loop {
                let msg = tokio::time::timeout_at(deadline, rx.recv())
                    .await
                    .map_err(|_| {
                        HarnessError::Protocol(format!(
                            "OpenCode did not report the compaction's outcome within {}s of admitting it",
                            wait.as_secs()
                        ))
                    })?;
                let event = match msg {
                    Some(BusMsg::Event(event)) => event,
                    Some(BusMsg::Disconnected) | None => {
                        return Err(HarnessError::Protocol(
                            "OpenCode's event stream ended while the compaction was running"
                                .into(),
                        ));
                    }
                    Some(_) => continue,
                };
                let props = event.get("properties").unwrap_or(&Value::Null);
                if props.get("sessionID").and_then(Value::as_str) != Some(session_id) {
                    continue;
                }
                match event.get("type").and_then(Value::as_str) {
                    Some("luna.compaction") => {
                        let input = props.get("inputID").and_then(Value::as_str);
                        match props.get("phase").and_then(Value::as_str) {
                            Some("started") if input == Some(inbox_id.as_str()) => started = true,
                            Some("ended") if started => {
                                tracing::info!(
                                    target: "luna_harness::opencode",
                                    "compaction {inbox_id} ended for {session_id}"
                                );
                                return Ok(());
                            }
                            Some("failed") if started || input == Some(inbox_id.as_str()) => {
                                let error = props
                                    .get("error")
                                    .and_then(Value::as_str)
                                    .filter(|e| !e.is_empty())
                                    .unwrap_or("no reason given");
                                return Err(HarnessError::RejectedBeforeAcceptance(format!(
                                    "OpenCode could not compact this session: {error}"
                                )));
                            }
                            _ => {}
                        }
                    }
                    Some("luna.turn.ended") if started => {
                        return Err(HarnessError::Protocol(
                            "OpenCode's compaction run ended without reporting completion".into(),
                        ));
                    }
                    _ => {}
                }
            }
        }
        .await;
        bus.abort();
        result
    }
}

// ---------------------------------------------------------------------------
// Live run servers: a compaction runs on the chat's own server when one is up
// ---------------------------------------------------------------------------

/// Servers of live runs by OpenCode session id. A compaction (or any other
/// out-of-turn control) for a session with a live run must go to that
/// run's server: a second server on the same session would race it.
#[derive(Clone, Default)]
pub(super) struct LiveServers(Arc<std::sync::Mutex<HashMap<String, (u64, Server)>>>);

impl LiveServers {
    pub(super) fn register(&self, session_id: &str, server: &Server) -> LiveServerGuard {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TOKENS: AtomicU64 = AtomicU64::new(1);
        let token = TOKENS.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut live) = self.0.lock() {
            live.insert(session_id.to_owned(), (token, server.detached()));
        }
        LiveServerGuard {
            servers: self.clone(),
            session_id: session_id.to_owned(),
            token,
        }
    }

    /// A non-owning handle on the live run's server for `session_id`.
    pub(super) fn get(&self, session_id: &str) -> Option<Server> {
        let live = self.0.lock().ok()?;
        live.get(session_id).map(|(_, server)| server.detached())
    }
}

/// Unregisters the run's server when the run ends (every exit path).
pub(super) struct LiveServerGuard {
    servers: LiveServers,
    session_id: String,
    token: u64,
}

impl Drop for LiveServerGuard {
    fn drop(&mut self) {
        if let Ok(mut live) = self.servers.0.lock()
            && live
                .get(&self.session_id)
                .is_some_and(|(token, _)| *token == self.token)
        {
            live.remove(&self.session_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Frame normalization: usage, model and compaction families
// ---------------------------------------------------------------------------

/// Catalog-invalidating global events (no `sessionID`): the run refreshes
/// its context windows and image capabilities.
pub(super) fn v2_global_frames(kind: &str) -> Option<Vec<Value>> {
    matches!(
        kind,
        "model.updated" | "provider.updated" | "integration.updated" | "models-dev.refreshed"
    )
    .then(|| vec![json!({ "type": "luna.catalog.changed", "properties": {} })])
}

/// `provider/model` from a v2 `Model.Ref`.
pub(super) fn model_key(model: &Value) -> Option<String> {
    let provider = model.get("providerID").and_then(Value::as_str)?;
    let id = model.get("id").and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

fn model_variant(model: &Value) -> Option<String> {
    model
        .get("variant")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Accounting frames for kinds other families also claim (step and
/// execution lifecycle). They are emitted AHEAD of the family frames, so a
/// turn's accounting is recorded before its idle frame settles the turn.
pub(super) fn v2_accounting_frames(kind: &str, data: &Value) -> Vec<Value> {
    let session = data.get("sessionID").cloned().unwrap_or(Value::Null);
    let message = data
        .get("assistantMessageID")
        .cloned()
        .unwrap_or(Value::Null);
    match kind {
        "session.step.started" => vec![json!({
            "type": "luna.step.started",
            "properties": {
                "sessionID": session,
                "messageID": message,
                "model": data.get("model").and_then(model_key),
                "variant": data.get("model").and_then(model_variant),
            }
        })],
        "session.step.ended" | "session.step.failed" => match data.get("tokens") {
            Some(tokens) if tokens.is_object() => vec![json!({
                "type": "luna.step.ended",
                "properties": {
                    "sessionID": session,
                    "messageID": message,
                    "tokens": tokens,
                    "cost": data.get("cost").cloned().unwrap_or(Value::Null),
                }
            })],
            _ => Vec::new(),
        },
        "session.execution.succeeded"
        | "session.execution.failed"
        | "session.execution.interrupted" => vec![json!({
            "type": "luna.turn.ended",
            "properties": { "sessionID": session }
        })],
        _ => Vec::new(),
    }
}

/// The usage family proper (kinds no other family claims): running totals,
/// model selection and the compaction lifecycle.
pub(super) fn v2_usage_frames(kind: &str, event: &Value, data: &Value) -> Option<Vec<Value>> {
    let session = data.get("sessionID").cloned().unwrap_or(Value::Null);
    let frames = match kind {
        "session.usage.updated" => match data.get("tokens") {
            Some(tokens) if tokens.is_object() => vec![json!({
                "type": "luna.usage.totals",
                "properties": {
                    "sessionID": session,
                    "tokens": tokens,
                    "cost": data.get("cost").cloned().unwrap_or(Value::Null),
                }
            })],
            _ => Vec::new(),
        },
        "session.model.selected" => vec![json!({
            "type": "luna.model.selected",
            "properties": {
                "sessionID": session,
                "model": data.get("model").and_then(model_key),
                "variant": data.get("model").and_then(model_variant),
            }
        })],
        "session.compaction.started" | "session.compaction.ended" | "session.compaction.failed" => {
            let phase = kind.rsplit('.').next().unwrap_or_default();
            let error = data.get("error").map(|error| {
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .or_else(|| error.get("type").and_then(Value::as_str))
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string())
            });
            vec![json!({
                "type": "luna.compaction",
                "properties": {
                    "sessionID": session,
                    "phase": phase,
                    "reason": data.get("reason").cloned().unwrap_or(Value::Null),
                    "inputID": data.get("inputID").cloned().unwrap_or(Value::Null),
                    "eventID": event.get("id").cloned().unwrap_or(Value::Null),
                    "error": error,
                }
            })]
        }
        // The summary stream is not shown (no LUNA surface for it).
        "session.compaction.delta" => Vec::new(),
        _ => return None,
    };
    Some(frames)
}

// ---------------------------------------------------------------------------
// The run's usage state
// ---------------------------------------------------------------------------

/// `TokenUsage.Info` as LUNA's split. OpenCode's `output` excludes
/// reasoning; LUNA's `output` includes it (`reasoning` is a subset).
pub(super) fn usage_tokens(tokens: &Value) -> UsageTokens {
    let n = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let reasoning = n("/reasoning");
    UsageTokens {
        uncached_input: n("/input"),
        cached_input: n("/cache/read"),
        cache_creation: n("/cache/write"),
        output: n("/output").saturating_add(reasoning),
        reasoning,
    }
}

/// Context held after one provider request: its whole prompt (uncached,
/// cache read and cache write) plus its visible output.
pub(super) fn step_context_tokens(tokens: &Value) -> u64 {
    let n = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    n("/input")
        .saturating_add(n("/cache/read"))
        .saturating_add(n("/cache/write"))
        .saturating_add(n("/output"))
}

/// What the run requested of the session's runtime.
#[derive(Debug, Clone, Default)]
pub(super) struct RequestedRuntime {
    pub(super) model: String,
    pub(super) variant: Option<String>,
    /// Why a requested variant was not sent (OCF-101).
    pub(super) dropped: Option<String>,
}

/// Per-run usage, model and compaction bookkeeping for the root session.
#[derive(Debug, Default)]
pub(super) struct UsageState {
    /// `provider/model` → `limit.context`.
    windows: HashMap<String, u64>,
    /// `provider/model` → `limit.output`.
    output_limits: HashMap<String, u64>,
    /// The session's model as last read back or selected.
    pub(super) effective_model: Option<String>,
    /// The model of the step in flight.
    step_model: Option<String>,
    step_message: Option<String>,
    turn_tokens: UsageTokens,
    turn_steps: u64,
    saw_usage: bool,
    totals: Option<(Option<f64>, UsageTokens)>,
    /// The auto-compaction in flight (`opencode-auto:{session}:{event}`).
    compaction: Option<String>,
}

impl UsageState {
    pub(super) fn new(providers: &ProviderCatalog) -> Self {
        let mut state = Self::default();
        state.set_catalog(providers);
        state
    }

    pub(super) fn set_catalog(&mut self, providers: &ProviderCatalog) {
        let limits = |pick: fn(&ProviderLimit) -> Option<u64>| -> HashMap<String, u64> {
            providers
                .all
                .iter()
                .flatten()
                .flat_map(|provider| {
                    provider
                        .models
                        .iter()
                        .flat_map(|models| models.iter())
                        .filter_map(move |(id, model)| {
                            Some((
                                format!("{}/{}", provider.id.as_deref()?, id),
                                pick(&model.limit).filter(|n| *n > 0)?,
                            ))
                        })
                })
                .collect()
        };
        self.windows = limits(|limit| limit.context);
        self.output_limits = limits(|limit| limit.output);
    }

    fn window(&self) -> Option<u64> {
        self.step_model
            .as_ref()
            .or(self.effective_model.as_ref())
            .and_then(|model| self.windows.get(model).copied())
    }

    fn result_report(&mut self, session_id: &str) -> AgentEvent {
        let model = self
            .step_model
            .clone()
            .or_else(|| self.effective_model.clone());
        let (cost, tokens) = self.totals.unwrap_or((None, UsageTokens::default()));
        let models = match (&model, self.totals.is_some()) {
            (Some(model), true) => vec![ModelAccounting {
                model: model.clone(),
                canonical_model: None,
                tokens,
                cost_usd: cost,
                context_window: self.windows.get(model).copied(),
                max_output_tokens: self.output_limits.get(model).copied(),
                web_search_requests: None,
            }],
            _ => Vec::new(),
        };
        let report = ResultAccounting {
            provider: UsageProvider::OpenCodeGo,
            native_session_id: Some(session_id.to_owned()),
            observed_at_ms: now_ms(),
            result_index: None,
            origin: None,
            turn_tokens: self.turn_tokens,
            cumulative_cost_usd: cost,
            models,
            duration_ms: None,
            duration_api_ms: None,
            num_turns: Some(self.turn_steps),
        };
        self.turn_tokens = UsageTokens::default();
        self.turn_steps = 0;
        self.saw_usage = false;
        AgentEvent::ProviderAccounting {
            report: Box::new(ProviderAccountingReport::Result(report)),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn readback_event(
    session_id: &str,
    source: RuntimeReadbackSource,
    requested: &RequestedRuntime,
    effective_model: Option<String>,
    effective_variant: Option<String>,
    detail: Option<String>,
) -> AgentEvent {
    let matches = effective_model.as_deref() == Some(requested.model.as_str())
        && (requested.variant.is_none() || effective_variant == requested.variant)
        && requested.dropped.is_none();
    let detail = match (requested.dropped.clone(), detail) {
        (Some(dropped), Some(detail)) => Some(format!("{dropped}; {detail}")),
        (dropped, detail) => dropped.or(detail),
    };
    AgentEvent::ProviderAccounting {
        report: Box::new(ProviderAccountingReport::Runtime(RuntimeReadback {
            provider: UsageProvider::OpenCodeGo,
            native_session_id: Some(session_id.to_owned()),
            observed_at_ms: now_ms(),
            source,
            requested_model: Some(requested.model.clone()),
            effective_model,
            requested_effort: requested.variant.clone(),
            effective_effort: effective_variant,
            matches,
            detail,
        })),
    }
}

/// Read the session's model back after `POST …/model` (OCF-104). A session
/// that previously ran another model reads back as a live switch. A failed
/// readback is reported as unconfirmed, never as a match.
pub(super) async fn model_readback(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    requested: &RequestedRuntime,
    previous: Option<&str>,
    usage: &mut UsageState,
) -> AgentEvent {
    let source = if previous.is_some_and(|previous| previous != requested.model) {
        RuntimeReadbackSource::LiveSwitch
    } else {
        RuntimeReadbackSource::Launch
    };
    match server.session_info(session_id, dir).await {
        Ok(info) => {
            let model = info.get("model");
            let effective = model.and_then(model_key);
            let variant = model.and_then(model_variant);
            usage.effective_model = effective.clone();
            let detail = effective
                .is_none()
                .then(|| "OpenCode's session readback named no model".to_owned());
            readback_event(session_id, source, requested, effective, variant, detail)
        }
        Err(error) => readback_event(
            session_id,
            source,
            requested,
            None,
            None,
            Some(format!("readback failed: {error}")),
        ),
    }
}

/// Everything [`handle_accounting`] needs from the run loop.
pub(super) struct AccountingCtx<'a> {
    pub(super) kind: &'a str,
    pub(super) props: &'a Value,
    pub(super) session_id: &'a str,
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) usage: &'a mut UsageState,
    pub(super) pending_usage: &'a mut Option<AgentEvent>,
}

/// Route one internal usage payload (`luna.*`, from [`v2_accounting_frames`]
/// / [`v2_usage_frames`] / [`v2_global_frames`]). `None` = not a usage
/// payload; the caller routes it as before. Child sessions' usage is left
/// to the subagent path.
pub(super) async fn handle_accounting(ctx: AccountingCtx<'_>) -> Option<BusOutcome> {
    let AccountingCtx {
        kind,
        props,
        session_id,
        server,
        dir,
        event_tx,
        usage,
        pending_usage,
    } = ctx;
    if !kind.starts_with("luna.") {
        return None;
    }
    if kind == "luna.catalog.changed" {
        // One read, no boot-race polling: never stall the event loop.
        match server.provider_catalog_once(dir).await {
            Ok(catalog) if catalog.all.as_ref().is_some_and(|all| !all.is_empty()) => {
                usage.set_catalog(&catalog);
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(
                target: "luna_harness::opencode",
                "catalog refresh after an update event failed: {error}"
            ),
        }
        return Some(BusOutcome::Continue);
    }
    if props.get("sessionID").and_then(Value::as_str) != Some(session_id) {
        return Some(BusOutcome::Continue);
    }
    let mut events = Vec::new();
    match kind {
        "luna.step.started" => {
            usage.step_message = props
                .get("messageID")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(model) = props.get("model").and_then(Value::as_str) {
                usage.step_model = Some(model.to_owned());
            }
        }
        "luna.step.ended" => {
            let tokens = props.get("tokens").unwrap_or(&Value::Null);
            let step = usage_tokens(tokens);
            usage.turn_tokens.add(step);
            usage.turn_steps += 1;
            usage.saw_usage = true;
            // A step that failed before the provider measured anything
            // carries zeros: no measurement, so the ring keeps its value.
            let context = step_context_tokens(tokens);
            if context > 0 {
                events.push(AgentEvent::ContextUsage {
                    tokens: Some(context),
                    window: usage.window(),
                });
            }
            let turn = usage.turn_tokens;
            *pending_usage = Some(AgentEvent::Usage {
                input_tokens: turn
                    .uncached_input
                    .saturating_add(turn.cached_input)
                    .saturating_add(turn.cache_creation),
                output_tokens: turn.output,
            });
        }
        "luna.usage.totals" => {
            let tokens = props.get("tokens").unwrap_or(&Value::Null);
            let cost = props.get("cost").and_then(Value::as_f64);
            usage.totals = Some((cost, usage_tokens(tokens)));
            usage.saw_usage = true;
        }
        "luna.model.selected" => {
            if let Some(model) = props.get("model").and_then(Value::as_str) {
                usage.effective_model = Some(model.to_owned());
            }
        }
        "luna.turn.ended" => {
            if usage.saw_usage {
                events.push(usage.result_report(session_id));
            }
        }
        "luna.compaction" => {
            // A manual compaction is owned (and settled) by the compaction
            // control; only automatic ones are run lifecycle (OCF-110).
            if props.get("reason").and_then(Value::as_str) == Some("auto") {
                let turn_id = usage
                    .step_message
                    .clone()
                    .unwrap_or_else(|| "turn".to_owned());
                let operation = |usage: &UsageState| usage.compaction.clone();
                let status = match props.get("phase").and_then(Value::as_str) {
                    Some("started") => {
                        let event_id = props
                            .get("eventID")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(ascending_message_id);
                        usage.compaction = Some(format!("opencode-auto:{session_id}:{event_id}"));
                        Some(ContextOperationStatus::Running)
                    }
                    Some("ended") => Some(ContextOperationStatus::Completed),
                    Some("failed") => Some(ContextOperationStatus::Failed),
                    _ => None,
                };
                if let (Some(status), Some(operation_id)) = (status, operation(usage)) {
                    events.push(AgentEvent::ContextCompaction {
                        operation_id,
                        status,
                        thread_id: session_id.to_owned(),
                        turn_id,
                    });
                    if status != ContextOperationStatus::Running {
                        usage.compaction = None;
                    }
                }
            }
        }
        _ => {}
    }
    Some(forward(event_tx, events).await)
}
