//! LUNA's runtime contract and engine-executed LUNA tools on the OpenCode 2
//! wire (OC-13), delivered the way the Claude adapter delivers them
//! ([`crate::claude::luna_tools`]):
//!
//! - **Attachment.** The SAME per-process, loopback-only `luna` MCP endpoint
//!   the Codex and Claude adapters attach ([`crate::codex::luna_mcp`]), named
//!   in one inline config layer: `OPENCODE_CONFIG_CONTENT` on the spawned
//!   server. It is OpenCode's highest-priority config layer, merged over the
//!   user's own config and written nowhere (`oc2:core/src/config.ts`), so the
//!   user's MCP connections (Executor) are untouched and a user-defined
//!   `luna` server is shadowed. The entry is `remote` with `oauth: false` and
//!   `codemode: false`: with Code Mode on (its default) the tools would hide
//!   behind Code Mode's `execute`, and no root tool call would name them.
//! - **Secret.** The bearer token rides `LUNA_MCP_BEARER_TOKEN` in the server
//!   environment; the header is `Bearer {env:LUNA_MCP_BEARER_TOKEN}`, which
//!   OpenCode substitutes, so the config text carries no credential. Agent
//!   tool processes inherit the server environment: the evidence fence, not
//!   secrecy, refuses direct calls (Claude's posture off Unix).
//! - **Allowlist.** The root session's rules (merged after the agent's, last
//!   match wins) allow exactly the attached `luna_<tool>` tools, and the
//!   inline layer denies `luna_*` for every agent otherwise. OpenCode 2.0.22
//!   copies a parent's session rules into each child session it creates
//!   (`oc2:core/src/session.ts`, "Children inherit metadata and
//!   permissions"), so a native subagent still SEES the attached tools: the
//!   deny only covers sessions without LUNA's rules. Refusing a subagent's
//!   call is the evidence fence's job, exactly as for Claude (verified live:
//!   the child's call is refused and the executor never runs).
//! - **Evidence.** OpenCode sends `_meta["ai.opencode/sessionID"]` with every
//!   MCP `tools/call`, but no call id. A call is accepted only when that id is
//!   the ROOT session and an unconsumed root tool call named `luna_<the same
//!   tool>` was seen on the bus in the current turn: `session.tool.input.
//!   started` names it and `session.tool.called` carries its input. Each
//!   observed call authorizes one MCP call (exact input first, else the
//!   oldest); a call racing ahead of its frames is parked for a short window.
//!   Turn ids are minted per posted prompt and announced as
//!   [`AgentEvent::ProviderTurnStarted`] so the engine's turn fence can claim
//!   the same call. The shared [`luna_mcp::dispatch`] fence (attached tool,
//!   call id claimed once per turn, interrupt) and the engine executor
//!   (scopes, exact one-use approval, receipts) then run unchanged.
//! - **Instructions.** The contract's sections (LUNA core, chat role, context,
//!   a side chat's frozen seed) ride the session's `luna` instruction entry
//!   (`PUT /api/experimental/session/{id}/instructions/entries/luna`), never a
//!   user-message prefix. OpenCode renders it as `<context key="luna">` from
//!   the next step and copies it on fork.
//! - **Verification (PD-10).** [`RuntimeDelivery::OpenCodeMcpTools`] is
//!   reported only for an OpenCode version whose live API contract has the
//!   delivery routes and on which no recent run failed its readback. Every
//!   run reads the attachment back before its first prompt (`GET /api/mcp`
//!   shows `luna` connected, the endpoint answered `tools/list`, the
//!   instruction entry reads back); a failure warns (`ProviderNotice`) and
//!   withdraws delivery for that version for ten minutes, so contracts
//!   compile as `Unsupported` meanwhile (and side chats fall back to their
//!   quoted seed); the next run after that re-verifies.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use luna_proto::{LunaAgentRuntimeContract, RuntimeDelivery};

use super::*;
use crate::LunaToolExecutor;
use crate::codex::LunaCallIds;
use crate::codex::luna_mcp::{
    self, LunaMcpCall, LunaMcpEndpoint, LunaToolReply, LunaToolRequest, LunaToolScope,
};

/// OpenCode's MCP tool naming: `<server>_<tool>` (`oc2:core/src/tool/mcp.ts`).
const TOOL_PREFIX: &str = "luna_";
/// `_meta` key OpenCode 2 sets on every MCP `tools/call` it makes for a
/// session (`oc2:core/src/mcp/client.ts`).
const SESSION_META: &str = "ai.opencode/sessionID";
/// The session instruction entry LUNA owns (`api/luna` server-side).
const INSTRUCTION_KEY: &str = "luna";
/// Inherited config layer this adapter merges into instead of replacing.
const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";
/// How long a call may wait for its tool frames to arrive on the bus.
const PARK_WINDOW: Duration = Duration::from_secs(5);
const MAX_PARKED: usize = 16;
const MAX_OBSERVED_PER_TURN: usize = 4_096;
/// Bound on the pre-prompt attachment readback (the loopback MCP connects in
/// well under a second; the location boots first).
const READBACK_BUDGET: Duration = Duration::from_secs(10);
const READBACK_POLL: Duration = Duration::from_millis(200);
/// How long a failed readback withdraws delivery before it is re-verified.
const DELIVERY_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
const NOT_ATTACHED: &str = "LUNA tool is not attached to this task or turn";
const UNMATCHED: &str = "LUNA tool call does not match a tool call in this OpenCode turn";

/// The routes LUNA tool delivery uses outside the boot contract (method,
/// OpenAPI path). Experimental, so their absence degrades delivery only.
const DELIVERY_ROUTES: &[(&str, &str)] = &[
    (
        "put",
        "/api/experimental/session/{sessionID}/instructions/entries/{key}",
    ),
    (
        "get",
        "/api/experimental/session/{sessionID}/instructions/entries",
    ),
    ("get", "/api/mcp"),
];

// ---------------------------------------------------------------------------
// Per-version delivery verdicts (PD-10)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum DeliveryVerdict {
    /// The live contract has the delivery routes; no run reported a failure.
    Ready,
    /// The version's contract lacks the delivery routes: final.
    Unavailable(String),
    /// A run's readback failed. Delivery stays withdrawn until `retry_at`,
    /// when the next contract promises it again and that run re-verifies
    /// (a slow machine's missed readback must not cost the whole session).
    Failed { reason: String, retry_at: Instant },
}

fn verdicts() -> &'static Mutex<HashMap<String, DeliveryVerdict>> {
    static VERDICTS: std::sync::OnceLock<Mutex<HashMap<String, DeliveryVerdict>>> =
        std::sync::OnceLock::new();
    VERDICTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn version_key(version: &str) -> String {
    version.trim().trim_start_matches('v').to_owned()
}

/// `~` and `/` escaped for a JSON pointer segment.
fn pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Record whether a version's live API contract carries the delivery routes
/// (called once per version by the boot contract check). A failure a run
/// already recorded for the version is never cleared here.
pub(super) fn record_delivery_contract(version: &str, spec: &Value) {
    let missing: Vec<String> = DELIVERY_ROUTES
        .iter()
        .filter(|(method, path)| {
            spec.pointer(&format!("/paths/{}/{method}", pointer_segment(path)))
                .is_none()
        })
        .map(|(method, path)| format!("{} {path}", method.to_uppercase()))
        .collect();
    let verdict = if missing.is_empty() {
        DeliveryVerdict::Ready
    } else {
        DeliveryVerdict::Unavailable(format!(
            "OpenCode {version} lacks LUNA's tool delivery routes ({})",
            missing.join(", ")
        ))
    };
    if let DeliveryVerdict::Unavailable(reason) = &verdict {
        tracing::warn!(target: "luna_harness::opencode", "{reason}; LUNA tools stay unavailable");
    }
    let Ok(mut verdicts) = verdicts().lock() else {
        return;
    };
    let key = version_key(version);
    if !matches!(verdicts.get(&key), Some(DeliveryVerdict::Failed { .. })) {
        verdicts.insert(key, verdict);
    }
}

/// A run could not verify its attachment: withdraw delivery for the version.
pub(super) fn record_delivery_failure(version: Option<&str>, reason: &str) {
    tracing::warn!(
        target: "luna_harness::opencode",
        version = version.unwrap_or("unknown"),
        "LUNA tool delivery failed its readback: {reason}"
    );
    let (Some(version), Ok(mut verdicts)) = (version, verdicts().lock()) else {
        return;
    };
    let key = version_key(version);
    if !matches!(verdicts.get(&key), Some(DeliveryVerdict::Unavailable(_))) {
        verdicts.insert(
            key,
            DeliveryVerdict::Failed {
                reason: reason.to_owned(),
                retry_at: Instant::now() + DELIVERY_RETRY_AFTER,
            },
        );
    }
}

/// The delivery a contract for this OpenCode version may promise.
pub(super) fn delivery_for_version(version: Option<&str>) -> RuntimeDelivery {
    let ready = version.is_some_and(|version| {
        match verdicts()
            .lock()
            .ok()
            .and_then(|verdicts| verdicts.get(&version_key(version)).cloned())
        {
            Some(DeliveryVerdict::Ready) => true,
            Some(DeliveryVerdict::Failed { retry_at, .. }) => Instant::now() >= retry_at,
            Some(DeliveryVerdict::Unavailable(_)) | None => false,
        }
    });
    if ready {
        RuntimeDelivery::OpenCodeMcpTools
    } else {
        RuntimeDelivery::Unsupported
    }
}

// ---------------------------------------------------------------------------
// Wire signals the run loop taps before normalization
// ---------------------------------------------------------------------------

/// Raw 2.x frames the run loop needs besides the normalized payloads: the
/// tool-call evidence for the LUNA fence, and catalog changes (OC-14).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum WireSignal {
    /// `session.tool.input.started` for a `luna_*` tool.
    ToolStarted {
        session: String,
        message: String,
        id: String,
        name: String,
    },
    /// `session.tool.called` (any tool; the bridge keeps only started ones).
    ToolCalled {
        session: String,
        message: String,
        id: String,
        input: Value,
    },
    /// `command.updated` or `skill.updated`.
    CatalogChanged,
}

/// Classify one raw 2.x `/api/event` frame.
pub(super) fn wire_signal(event: &Value) -> Option<WireSignal> {
    let kind = event.get("type").and_then(Value::as_str)?;
    if matches!(kind, "command.updated" | "skill.updated") {
        return Some(WireSignal::CatalogChanged);
    }
    let data = event.get("data")?;
    let field = |name: &str| {
        data.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    match kind {
        "session.tool.input.started" => {
            let name = field("name").filter(|name| name.starts_with(TOOL_PREFIX))?;
            Some(WireSignal::ToolStarted {
                session: field("sessionID")?,
                message: field("assistantMessageID")?,
                id: field("id")?,
                name,
            })
        }
        "session.tool.called" => Some(WireSignal::ToolCalled {
            session: field("sessionID")?,
            message: field("assistantMessageID")?,
            id: field("id")?,
            input: data.get("input").cloned().unwrap_or_else(|| json!({})),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Run preparation (before the server spawn)
// ---------------------------------------------------------------------------

/// What one run needs to deliver LUNA's contract: the bound endpoint (when
/// tools are attached), the session-loop bridge and the server environment.
pub(super) struct LunaRun {
    pub(super) endpoint: Option<LunaMcpEndpoint>,
    pub(super) bridge: OpencodeLunaBridge,
    pub(super) env: Vec<(String, String)>,
}

/// Take the engine's runtime contract out of the request and prepare its
/// delivery. `None`: no contract (or one with nothing to deliver).
pub(super) async fn prepare_luna_run(
    request: &mut RunRequest,
    executor: Option<&LunaToolExecutor>,
    interrupt: &CancellationToken,
) -> Result<Option<LunaRun>, HarnessError> {
    let Some(value) = request
        .model_options
        .remove(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION)
    else {
        return Ok(None);
    };
    let contract: LunaAgentRuntimeContract = serde_json::from_value(value).map_err(|error| {
        HarnessError::Protocol(format!("invalid LUNA runtime contract: {error}"))
    })?;
    if contract.context.harness != HarnessId::Opencode
        || !matches!(
            contract.delivery,
            RuntimeDelivery::OpenCodeMcpTools | RuntimeDelivery::Unsupported
        )
        || (!contract.tools.is_empty()
            && (contract.delivery != RuntimeDelivery::OpenCodeMcpTools || executor.is_none()))
    {
        return Err(HarnessError::Protocol(
            "OpenCode LUNA runtime contract advertises unsupported tools".into(),
        ));
    }
    let instructions = render_instructions(&contract);
    if contract.tools.is_empty() && instructions.is_empty() {
        return Ok(None);
    }
    let mut env: Vec<(String, String)> = contract
        .environment
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "LUNA_THREAD_ID" | "LUNA_PROJECT_ID" | "LUNA_ENVIRONMENT_ID" | "LUNA_MACHINE_ID"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let endpoint = match (contract.tools.is_empty(), executor) {
        (false, Some(_)) => {
            let endpoint = LunaMcpEndpoint::bind(&contract.tools).await?;
            register_secret(endpoint.token());
            env.push((luna_mcp::TOKEN_ENV.into(), endpoint.token().to_owned()));
            env.push((
                CONFIG_CONTENT_ENV.into(),
                config_content(
                    endpoint.url(),
                    std::env::var(CONFIG_CONTENT_ENV).ok().as_deref(),
                ),
            ));
            Some(endpoint)
        }
        _ => None,
    };
    Ok(Some(LunaRun {
        bridge: OpencodeLunaBridge {
            tools: endpoint
                .as_ref()
                .map(|_| {
                    contract
                        .tools
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            contract,
            executor: executor.cloned(),
            interrupt: interrupt.clone(),
            instructions,
            session_id: None,
            turn: None,
            started: HashMap::new(),
            call_ids: LunaCallIds::default(),
            parked: Vec::new(),
            closed: CancellationToken::new(),
        },
        endpoint,
        env,
    }))
}

/// The contract's instruction sections as one entry value (the same
/// rendering the Codex and Claude adapters use).
fn render_instructions(contract: &LunaAgentRuntimeContract) -> String {
    contract
        .instructions
        .iter()
        .map(|section| {
            format!(
                "[LUNA {:?}: {} | {}]\n{}",
                section.source, section.name, section.provenance, section.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// OpenCode's MCP tool / permission action name for one LUNA tool.
fn action_name(tool: &str) -> String {
    let tool: String = tool
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    format!("{TOOL_PREFIX}{tool}")
}

/// The inline config layer: LUNA's `luna` server, and `luna_*` denied for
/// every agent (the root session's rules re-allow the attached tools). An
/// inherited `OPENCODE_CONFIG_CONTENT` object is kept and extended.
pub(super) fn config_content(url: &str, inherited: Option<&str>) -> String {
    let mut config = inherited
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let server = json!({
        "type": "remote",
        "url": url,
        "headers": {
            "Authorization": format!("Bearer {{env:{}}}", luna_mcp::TOKEN_ENV)
        },
        "oauth": false,
        "codemode": false,
        // A parked LUNA approval waits for the user, not a transport
        // deadline (OpenCode's default execution wall is 12 h).
        "timeout": { "execution": luna_mcp::TOOL_TIMEOUT_SEC * 1_000 },
    });
    if !config.get("mcp").is_some_and(Value::is_object) {
        config["mcp"] = json!({});
    }
    if !config["mcp"].get("servers").is_some_and(Value::is_object) {
        config["mcp"]["servers"] = json!({});
    }
    config["mcp"]["servers"][luna_mcp::SERVER_NAME] = server;
    let deny = json!({ "action": format!("{TOOL_PREFIX}*"), "resource": "*", "effect": "deny" });
    match config.get_mut("permissions").and_then(Value::as_array_mut) {
        Some(rules) => rules.push(deny),
        None => config["permissions"] = json!([deny]),
    }
    config.to_string()
}

// ---------------------------------------------------------------------------
// Session-loop bridge
// ---------------------------------------------------------------------------

struct Turn {
    id: String,
    /// Root `luna_*` tool calls seen this turn and not yet consumed.
    observed: Vec<Observed>,
    seen: usize,
}

struct Observed {
    /// The chip id the bus feed uses (`<session>:<message>:<call id>`).
    call_id: String,
    tool: String,
    input: Value,
}

struct Parked {
    call: LunaMcpCall,
    deadline: Instant,
}

enum CallVerdict {
    Dispatch(Observed),
    Park,
    Reject(&'static str),
}

/// Session-loop state for one OpenCode run's LUNA contract and tools.
pub(super) struct OpencodeLunaBridge {
    contract: LunaAgentRuntimeContract,
    executor: Option<LunaToolExecutor>,
    interrupt: CancellationToken,
    instructions: String,
    /// Attached tool names (empty when no endpoint is bound).
    tools: Vec<String>,
    session_id: Option<String>,
    turn: Option<Turn>,
    /// Root `luna_*` tool calls whose input has not been called yet
    /// (`(message, call id)` → tool name without the prefix).
    started: HashMap<(String, String), String>,
    call_ids: LunaCallIds,
    parked: Vec<Parked>,
    /// Cancelled when the bridge (and so the run loop) is dropped: a call
    /// still parked on its LUNA approval is abandoned with the run, and the
    /// engine's shared resolver watch retires the card.
    closed: CancellationToken,
}

impl Drop for OpencodeLunaBridge {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

impl OpencodeLunaBridge {
    fn attaches_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    /// Append `luna_<tool>: allow` for exactly the attached tools to the
    /// root session's ruleset (after LUNA's mode rules: last match wins).
    pub(super) fn extend_rules(&self, rules: &mut Value) {
        let Some(rules) = rules.as_array_mut() else {
            return;
        };
        for tool in &self.tools {
            rules.push(json!({ "action": action_name(tool), "resource": "*", "effect": "allow" }));
        }
    }

    pub(super) fn bind_session(&mut self, session_id: &str) {
        if self.session_id.as_deref() != Some(session_id) {
            self.end_turn();
            self.started.clear();
            self.session_id = Some(session_id.to_owned());
        }
    }

    /// Before the first prompt: read the MCP attachment back, then deliver
    /// the instructions through the session's `luna` entry and read it back.
    /// `Err` names what failed; the run continues without the guarantee.
    pub(super) async fn deliver(
        &self,
        server: &Server,
        dir: Option<&str>,
        endpoint: Option<&LunaMcpEndpoint>,
    ) -> Result<(), String> {
        let session_id = self
            .session_id
            .as_deref()
            .ok_or_else(|| "no OpenCode session".to_owned())?;
        if let Some(endpoint) = endpoint {
            await_mcp_attachment(server, dir, endpoint).await?;
        }
        if !self.instructions.is_empty() {
            put_instructions(server, session_id, dir, &self.instructions).await?;
        }
        Ok(())
    }

    /// A prompt is about to be posted: mint its turn id. The announcement
    /// arms the engine's claim fence; forward it after `SessionStarted`.
    pub(super) fn begin_turn(&mut self) -> Option<AgentEvent> {
        self.end_turn();
        if !self.attaches_tools() {
            return None;
        }
        let thread_id = self.session_id.clone()?;
        let turn_id = format!("opencode-turn-{}", uuid::Uuid::new_v4().simple());
        self.turn = Some(Turn {
            id: turn_id.clone(),
            observed: Vec::new(),
            seen: 0,
        });
        Some(AgentEvent::ProviderTurnStarted { thread_id, turn_id })
    }

    /// The turn settled: unconsumed evidence and parked calls die with it.
    pub(super) fn end_turn(&mut self) {
        self.turn = None;
        for parked in std::mem::take(&mut self.parked) {
            reject(parked.call, NOT_ATTACHED);
        }
    }

    /// Track root tool-call evidence from one raw bus frame.
    pub(super) fn observe(&mut self, signal: WireSignal) {
        let Some(root) = self.session_id.as_deref() else {
            return;
        };
        match signal {
            WireSignal::ToolStarted {
                session,
                message,
                id,
                name,
            } if session == root => {
                if self.started.len() < MAX_OBSERVED_PER_TURN
                    && let Some(tool) = name.strip_prefix(TOOL_PREFIX)
                {
                    self.started.insert((message, id), tool.to_owned());
                }
            }
            WireSignal::ToolCalled {
                session,
                message,
                id,
                input,
            } if session == root => {
                let Some(tool) = self.started.remove(&(message.clone(), id.clone())) else {
                    return;
                };
                let Some(turn) = self.turn.as_mut() else {
                    return;
                };
                if turn.seen >= MAX_OBSERVED_PER_TURN {
                    return;
                }
                turn.seen += 1;
                turn.observed.push(Observed {
                    call_id: format!("{session}:{message}:{id}"),
                    tool,
                    input,
                });
                self.release_parked();
            }
            _ => {}
        }
    }

    /// One `tools/call` from the endpoint.
    pub(super) fn on_call(&mut self, call: LunaMcpCall) {
        match self.verdict(&call) {
            CallVerdict::Dispatch(observed) => self.dispatch(call, observed),
            CallVerdict::Reject(message) => reject(call, message),
            CallVerdict::Park if self.parked.len() < MAX_PARKED => self.parked.push(Parked {
                call,
                deadline: Instant::now() + PARK_WINDOW,
            }),
            CallVerdict::Park => reject(call, UNMATCHED),
        }
    }

    /// Earliest parked deadline, for the run loop's timer branch.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.parked.iter().map(|parked| parked.deadline).min()
    }

    /// Refuse parked calls whose tool frames never appeared in time.
    pub(super) fn expire(&mut self) {
        let now = Instant::now();
        let (expired, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.parked)
            .into_iter()
            .partition(|parked| parked.deadline <= now);
        self.parked = waiting;
        for parked in expired {
            reject(parked.call, UNMATCHED);
        }
    }

    fn verdict(&mut self, call: &LunaMcpCall) -> CallVerdict {
        let session = call
            .meta
            .get(SESSION_META)
            .and_then(Value::as_str)
            .unwrap_or_default();
        // A child session (native subagent) or an unattributed caller is
        // never LUNA's root conversation.
        if session.is_empty() || self.session_id.as_deref() != Some(session) {
            return CallVerdict::Reject(NOT_ATTACHED);
        }
        // Between turns a call waits for the turn and its tool call; it is
        // never run without them.
        let Some(turn) = self.turn.as_mut() else {
            return CallVerdict::Park;
        };
        let exact = turn
            .observed
            .iter()
            .position(|seen| seen.tool == call.name && seen.input == call.arguments);
        let index = exact.or_else(|| turn.observed.iter().position(|seen| seen.tool == call.name));
        match index {
            Some(index) => CallVerdict::Dispatch(turn.observed.remove(index)),
            None => CallVerdict::Park,
        }
    }

    fn dispatch(&mut self, call: LunaMcpCall, observed: Observed) {
        let (Some(thread_id), Some(turn)) = (self.session_id.as_deref(), self.turn.as_ref()) else {
            reject(call, NOT_ATTACHED);
            return;
        };
        let LunaMcpCall {
            name,
            arguments,
            reply,
            ..
        } = call;
        luna_mcp::dispatch(
            LunaToolRequest {
                thread_id: Some(thread_id),
                turn_id: Some(turn.id.as_str()),
                call_id: &observed.call_id,
                foreign_namespace: false,
                name: &name,
                arguments,
            },
            LunaToolScope {
                thread_id,
                active_turn_id: Some(turn.id.as_str()),
                runtime_contract: Some(&self.contract),
                invoke_luna_tool: self.executor.as_ref(),
                call_ids: &mut self.call_ids,
                interrupt: &self.interrupt,
                closed: &self.closed,
            },
            LunaToolReply::Mcp(reply),
        );
    }

    fn release_parked(&mut self) {
        for parked in std::mem::take(&mut self.parked) {
            if parked.call.reply.is_closed() {
                continue;
            }
            match self.verdict(&parked.call) {
                CallVerdict::Park => self.parked.push(parked),
                CallVerdict::Dispatch(observed) => self.dispatch(parked.call, observed),
                CallVerdict::Reject(message) => reject(parked.call, message),
            }
        }
    }
}

fn reject(call: LunaMcpCall, message: &str) {
    let _ = call.reply.send((false, message.to_owned()));
}

/// `GET /api/mcp` until `luna` is connected AND the endpoint answered a
/// `tools/list` (the catalog reached OpenCode), bounded.
async fn await_mcp_attachment(
    server: &Server,
    dir: Option<&str>,
    endpoint: &LunaMcpEndpoint,
) -> Result<(), String> {
    let deadline = Instant::now() + READBACK_BUDGET;
    loop {
        let last = match server.get_json("/api/mcp", dir).await {
            Ok(value) => {
                let rows = unwrap_data(value);
                let row = rows.as_array().and_then(|rows| {
                    rows.iter().find(|row| {
                        row.get("name").and_then(Value::as_str) == Some(luna_mcp::SERVER_NAME)
                    })
                });
                let status = row
                    .and_then(|row| row.pointer("/status/status"))
                    .and_then(Value::as_str)
                    .unwrap_or("not listed");
                match status {
                    "connected" if endpoint.catalog_reads() > 0 => return Ok(()),
                    "failed" | "needs_auth" | "disabled" => {
                        return Err(format!("OpenCode reports the luna MCP server as {status}"));
                    }
                    _ => status.to_owned(),
                }
            }
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "the luna MCP server was not attached within {}s ({last}; catalog reads: {})",
                READBACK_BUDGET.as_secs(),
                endpoint.catalog_reads()
            ));
        }
        tokio::time::sleep(READBACK_POLL).await;
    }
}

/// `PUT …/instructions/entries/luna {value}` (204), then read it back.
async fn put_instructions(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    text: &str,
) -> Result<(), String> {
    let entries = format!("/api/experimental/session/{session_id}/instructions/entries");
    let path = format!("{entries}/{INSTRUCTION_KEY}");
    let (status, body) = server
        .exchange(
            reqwest::Method::PUT,
            &path,
            dir,
            Some(&json!({ "value": text })),
            Some(CALL_TIMEOUT),
        )
        .await
        .map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(http_error_message("PUT", &path, status, &body));
    }
    let listed = server
        .get_json(&entries, dir)
        .await
        .map_err(|error| error.to_string())?;
    let present = unwrap_data(listed).as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry.get("key").and_then(Value::as_str) == Some(INSTRUCTION_KEY)
                && entry.get("value").and_then(Value::as_str) == Some(text)
        })
    });
    if present {
        Ok(())
    } else {
        Err("the luna instruction entry did not read back".into())
    }
}

/// Sleep until a parked call's deadline, or forever when nothing is parked.
pub(super) async fn parked_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Next endpoint call. A closed endpoint is retired so the run loop does not
/// spin on it; without an endpoint this pends forever.
pub(super) async fn next_call(endpoint: &mut Option<LunaMcpEndpoint>) -> Option<LunaMcpCall> {
    let call = LunaMcpEndpoint::next_call(endpoint).await;
    if call.is_none() {
        *endpoint = None;
    }
    call
}

/// A transient warning beside the working indicator (never a turn failure).
pub(super) fn delivery_notice(reason: &str) -> AgentEvent {
    AgentEvent::ProviderNotice {
        notice: luna_proto::ProviderNotice {
            kind: luna_proto::ProviderNoticeKind::Warning,
            message: format!("LUNA tools are unavailable in this OpenCode session: {reason}"),
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        },
    }
}
