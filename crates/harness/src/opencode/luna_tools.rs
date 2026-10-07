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
//! - **Names.** OpenCode names an MCP tool `<server>_<tool>`
//!   (`oc2:core/src/tool/mcp.ts`), so the endpoint lists an engine tool that
//!   already starts with `luna_` without that prefix (`luna_status` is listed
//!   as `status`): the model sees `luna_status`, `luna_get_workspace`, never
//!   `luna_luna_status`. The listed name maps back to the engine's canonical
//!   name for the evidence fence, the executor and the tool chips, so every
//!   surface shows the same names as Codex and Claude.
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
//!   the next step and copies it on fork. Entries persist on the session, so
//!   a resumed (or forked) session whose run delivers no contract has its
//!   inherited `luna` entry removed: it would still tell the model that LUNA
//!   tools are attached. A delivering run replaces it and reads back exactly
//!   one current entry.
//! - **Verification (PD-10).** Like Claude's `ClaudeMcpTools`, a contract
//!   compiles optimistically: [`RuntimeDelivery::OpenCodeMcpTools`] is
//!   reported until this process learns otherwise, so the very first turn
//!   after LUNA starts carries the tools and the contract without waiting for
//!   an earlier boot. Nothing reaches the model before the run verifies it:
//!   the boot contract must have the delivery routes, and the attachment must
//!   read back (`GET /api/mcp` shows `luna` connected and the endpoint
//!   answered `tools/list`) BEFORE the session rules allow a single
//!   `luna_<tool>`; until then the inline deny hides them all. The first
//!   prompt then waits out OpenCode's debounced tool registration
//!   ([`TOOL_REGISTRATION_SETTLE`]), or the model's first step would see no
//!   LUNA tool at all. A version
//!   without the routes is final (`Unsupported`); a failed readback or
//!   instruction delivery warns (`ProviderNotice`) and withdraws delivery for
//!   that version for ten minutes, so contracts compile as `Unsupported`
//!   meanwhile (and side chats fall back to their quoted seed); the next run
//!   after that re-verifies.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use luna_proto::{
    LunaAgentRuntimeContract, RuntimeDelivery, RuntimeInstructionSource, RuntimeToolDefinition,
};

use super::*;
use crate::LunaToolExecutor;
use crate::codex::LunaCallIds;
use crate::codex::luna_mcp::{
    self, LunaMcpCall, LunaMcpEndpoint, LunaToolReply, LunaToolRequest, LunaToolScope,
};

/// OpenCode's MCP tool naming: `<server>_<tool>` (`oc2:core/src/tool/mcp.ts`),
/// for the `luna` server.
const TOOL_PREFIX: &str = "luna_";
/// Bound on the process-wide listed → canonical tool-name map.
const MAX_TOOL_ALIASES: usize = 256;
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
/// How long after `luna` reads back connected the first prompt waits.
/// OpenCode registers a server's tools for the model only when its debounced
/// (100 ms) `ToolsChanged` reconcile runs (`oc2:core/src/tool/mcp.ts`), with
/// nothing observable over HTTP; a step that starts sooner has none of the
/// LUNA tools ("No tool named luna_get_workspace", verified live on 2.0.22
/// with a prompt 70 ms after the connect).
pub(super) const TOOL_REGISTRATION_SETTLE: Duration = Duration::from_millis(400);
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
    // Retires a resumed session's stale entry when a run delivers nothing.
    (
        "delete",
        "/api/experimental/session/{sessionID}/instructions/entries/{key}",
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

/// Why delivery is withdrawn for this OpenCode version, or `None` while a
/// contract may promise it. An unchecked version (nothing booted yet in this
/// process, or no version known) is NOT withdrawn: contracts compile
/// optimistically, like Claude's, and the run verifies before the model sees
/// a tool.
pub(super) fn delivery_withdrawal(version: Option<&str>) -> Option<String> {
    let verdict = version.and_then(|version| {
        verdicts()
            .lock()
            .ok()
            .and_then(|verdicts| verdicts.get(&version_key(version)).cloned())
    });
    match verdict {
        None | Some(DeliveryVerdict::Ready) => None,
        Some(DeliveryVerdict::Failed { reason, retry_at }) => {
            (Instant::now() < retry_at).then_some(reason)
        }
        Some(DeliveryVerdict::Unavailable(reason)) => Some(reason),
    }
}

/// The delivery a contract for this OpenCode version may promise.
pub(super) fn delivery_for_version(version: Option<&str>) -> RuntimeDelivery {
    if delivery_withdrawal(version).is_none() {
        RuntimeDelivery::OpenCodeMcpTools
    } else {
        RuntimeDelivery::Unsupported
    }
}

// ---------------------------------------------------------------------------
// Tool names: what OpenCode sees vs the engine's canonical names
// ---------------------------------------------------------------------------

/// The name the `luna` endpoint lists for `tool` to OpenCode, which prefixes
/// `luna_` itself: a redundant `luna_` prefix is dropped (`luna_status` →
/// `status`, exposed as `luna_status`) unless the shorter name is itself an
/// attached tool, so listed names stay unique.
fn listed_name(tool: &str, attached: &[String]) -> String {
    match tool.strip_prefix(TOOL_PREFIX) {
        Some(short) if !short.is_empty() && !attached.iter().any(|other| other == short) => {
            short.to_owned()
        }
        _ => tool.to_owned(),
    }
}

/// OpenCode's MCP tool and permission action name for a listed tool name
/// (`oc2:core/src/tool/mcp.ts` `name`).
fn action_name(listed: &str) -> String {
    let listed: String = listed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    format!("{TOOL_PREFIX}{listed}")
}

/// Listed → canonical names this process attached (only the ones that
/// differ). Tool chips are typed without the run's contract at hand; the
/// engine's tool names are stable, so one map serves every run.
fn tool_aliases() -> &'static Mutex<HashMap<String, String>> {
    static ALIASES: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    ALIASES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_tool_aliases(tools: &[AttachedTool]) {
    let Ok(mut aliases) = tool_aliases().lock() else {
        return;
    };
    for tool in tools.iter().filter(|tool| tool.listed != tool.canonical) {
        if aliases.len() < MAX_TOOL_ALIASES || aliases.contains_key(&tool.listed) {
            aliases.insert(tool.listed.clone(), tool.canonical.clone());
        }
    }
}

/// The engine's name for a `luna` server tool as OpenCode reported it (the
/// part after `luna_`), so chips read `luna_status` like Codex's and
/// Claude's. Unknown names pass through.
pub(super) fn canonical_luna_tool(listed: &str) -> String {
    tool_aliases()
        .lock()
        .ok()
        .and_then(|aliases| aliases.get(listed).cloned())
        .unwrap_or_else(|| listed.to_owned())
}

/// One attached LUNA tool: the engine's canonical name and the name the
/// endpoint lists to OpenCode.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AttachedTool {
    canonical: String,
    listed: String,
}

fn attached_tools(tools: &[RuntimeToolDefinition]) -> Vec<AttachedTool> {
    let names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    names
        .iter()
        .map(|name| AttachedTool {
            canonical: name.clone(),
            listed: listed_name(name, &names),
        })
        .collect()
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
    let instructions = render_instructions(&contract, true);
    if contract.tools.is_empty() && instructions.is_empty() {
        return Ok(None);
    }
    let tools = attached_tools(&contract.tools);
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
            // The catalog OpenCode reads carries the listed names; calls
            // arrive under them and are mapped back in the bridge.
            let listed: Vec<RuntimeToolDefinition> = contract
                .tools
                .iter()
                .zip(&tools)
                .map(|(definition, tool)| RuntimeToolDefinition {
                    name: tool.listed.clone(),
                    ..definition.clone()
                })
                .collect();
            let endpoint = LunaMcpEndpoint::bind(&listed).await?;
            tracing::debug!(
                target: "luna_harness::opencode",
                tools = ?tools.iter().map(|tool| action_name(&tool.listed)).collect::<Vec<_>>(),
                "LUNA tools attached to OpenCode"
            );
            register_tool_aliases(&tools);
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
            tools: if endpoint.is_some() {
                tools
            } else {
                Vec::new()
            },
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
/// rendering the Codex and Claude adapters use). Without `tool_sections` the
/// tool guidance is left out (a run whose tools were withdrawn keeps its
/// identity, context and side-chat seed, but promises no tool).
fn render_instructions(contract: &LunaAgentRuntimeContract, tool_sections: bool) -> String {
    contract
        .instructions
        .iter()
        .filter(|section| tool_sections || section.source != RuntimeInstructionSource::Tool)
        .map(|section| {
            format!(
                "[LUNA {:?}: {} | {}]\n{}",
                section.source, section.name, section.provenance, section.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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
    /// Attached tools (empty when no endpoint is bound, or after the run
    /// withdrew them because delivery could not be verified).
    tools: Vec<AttachedTool>,
    session_id: Option<String>,
    turn: Option<Turn>,
    /// Root `luna_*` tool calls whose input has not been called yet
    /// (`(message, call id)` → canonical tool name).
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
    pub(super) fn attaches_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    /// Delivery could not be verified for this run: no `luna_<tool>` is
    /// allowed (the inline deny hides them all), no turn ids are minted,
    /// every endpoint call is refused, and the instruction entry keeps the
    /// identity, context and side-chat seed but drops the tool guidance.
    pub(super) fn withdraw_tools(&mut self) {
        if self.tools.is_empty() {
            return;
        }
        self.tools.clear();
        self.end_turn();
        self.instructions = render_instructions(&self.contract, false);
    }

    /// The attached tool an endpoint call names (`<listed>`), by its
    /// canonical name.
    fn canonical(&self, listed: &str) -> Option<&str> {
        self.tools
            .iter()
            .find(|tool| tool.listed == listed)
            .map(|tool| tool.canonical.as_str())
    }

    /// The attached tool a bus frame names (OpenCode's `luna_<listed>`
    /// action name), by its canonical name.
    fn canonical_for_action(&self, name: &str) -> Option<&str> {
        self.tools
            .iter()
            .find(|tool| action_name(&tool.listed) == name)
            .map(|tool| tool.canonical.as_str())
    }

    /// Append `luna_<tool>: allow` for exactly the attached tools, under the
    /// names OpenCode exposes, to the root session's ruleset (after LUNA's
    /// mode rules: last match wins).
    pub(super) fn extend_rules(&self, rules: &mut Value) {
        let Some(rules) = rules.as_array_mut() else {
            return;
        };
        for tool in &self.tools {
            rules.push(
                json!({ "action": action_name(&tool.listed), "resource": "*", "effect": "allow" }),
            );
        }
    }

    pub(super) fn bind_session(&mut self, session_id: &str) {
        if self.session_id.as_deref() != Some(session_id) {
            self.end_turn();
            self.started.clear();
            self.session_id = Some(session_id.to_owned());
        }
    }

    /// Before the first prompt (after the attachment was verified): deliver
    /// the instructions through the session's `luna` entry and read back
    /// exactly one current entry; with nothing to deliver, retire an entry
    /// an earlier run left. `Err` names what failed; the run continues
    /// without the guarantee.
    pub(super) async fn deliver_instructions(
        &self,
        server: &Server,
        dir: Option<&str>,
    ) -> Result<(), String> {
        let session_id = self
            .session_id
            .as_deref()
            .ok_or_else(|| "no OpenCode session".to_owned())?;
        if self.instructions.is_empty() {
            retire_instructions(server, session_id, dir).await;
            return Ok(());
        }
        put_instructions(server, session_id, dir, &self.instructions).await
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
                    && let Some(tool) = self.canonical_for_action(&name)
                {
                    let tool = tool.to_owned();
                    self.started.insert((message, id), tool);
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

    /// One `tools/call` from the endpoint, under its listed name: matched and
    /// dispatched by the engine's canonical name.
    pub(super) fn on_call(&mut self, mut call: LunaMcpCall) {
        let Some(canonical) = self.canonical(&call.name).map(str::to_owned) else {
            reject(call, NOT_ATTACHED);
            return;
        };
        call.name = canonical;
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
/// `tools/list` (the catalog reached OpenCode), bounded. Runs before the
/// session rules allow any LUNA tool. `Ok` is the earliest time the first
/// prompt may be posted ([`TOOL_REGISTRATION_SETTLE`] after the readback).
pub(super) async fn await_mcp_attachment(
    server: &Server,
    dir: Option<&str>,
    endpoint: &LunaMcpEndpoint,
) -> Result<Instant, String> {
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
                    "connected" if endpoint.catalog_reads() > 0 => {
                        return Ok(Instant::now() + TOOL_REGISTRATION_SETTLE);
                    }
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

fn entries_path(session_id: &str) -> String {
    format!("/api/experimental/session/{session_id}/instructions/entries")
}

/// The session's `luna` entry values (normally zero or one).
async fn luna_entries(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
) -> Result<Vec<String>, String> {
    let listed = server
        .get_json(&entries_path(session_id), dir)
        .await
        .map_err(|error| error.to_string())?;
    Ok(unwrap_data(listed)
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("key").and_then(Value::as_str) == Some(INSTRUCTION_KEY))
                .map(|entry| {
                    entry
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default())
}

/// `PUT …/instructions/entries/luna {value}` (204; replaces an earlier run's
/// entry), then read back exactly one `luna` entry with this value.
async fn put_instructions(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    text: &str,
) -> Result<(), String> {
    let path = format!("{}/{INSTRUCTION_KEY}", entries_path(session_id));
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
    let entries = luna_entries(server, session_id, dir).await?;
    match entries.as_slice() {
        [value] if value == text => Ok(()),
        [] => Err("the luna instruction entry did not read back".into()),
        [_] => Err("the luna instruction entry read back with a different value".into()),
        _ => Err(format!(
            "the session reads back {} luna instruction entries instead of one",
            entries.len()
        )),
    }
}

/// Remove the session's `luna` entry when an earlier run left one: a resumed
/// or forked session whose run delivers no contract must not keep telling
/// the model that LUNA tools are attached. Best effort (a server without the
/// routes, or a failed call, is logged, never a turn failure).
pub(super) async fn retire_instructions(server: &Server, session_id: &str, dir: Option<&str>) {
    let found = match luna_entries(server, session_id, dir).await {
        Ok(entries) => entries.len(),
        Err(error) => {
            tracing::debug!(target: "luna_harness::opencode", "luna instruction entries unreadable: {error}");
            return;
        }
    };
    if found == 0 {
        return;
    }
    let path = format!("{}/{INSTRUCTION_KEY}", entries_path(session_id));
    let outcome = server
        .exchange(
            reqwest::Method::DELETE,
            &path,
            dir,
            None,
            Some(CALL_TIMEOUT),
        )
        .await;
    match outcome {
        Ok((status, _)) if status.is_success() => tracing::info!(
            target: "luna_harness::opencode",
            "removed a stale luna instruction entry from a session that runs without LUNA's contract"
        ),
        Ok((status, body)) => tracing::warn!(
            target: "luna_harness::opencode",
            "could not remove a stale luna instruction entry: {}",
            http_error_message("DELETE", &path, status, &body)
        ),
        Err(error) => tracing::warn!(
            target: "luna_harness::opencode",
            "could not remove a stale luna instruction entry: {error}"
        ),
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
