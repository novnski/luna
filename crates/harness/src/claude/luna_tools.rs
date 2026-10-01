//! Engine-executed LUNA tools on the Claude wire (CC-07).
//!
//! The Claude CLI gets the SAME per-process, loopback-only `luna` MCP endpoint
//! the Codex adapter attaches ([`crate::codex::luna_mcp`]); only the CLI
//! attachment and the provider evidence differ:
//!
//! - **Attachment.** One inline `--mcp-config` entry that names only the `luna`
//!   server, plus `--allowedTools` for exactly the attached tools. Nothing is
//!   written to the user's Claude config, and normal runs never pass
//!   `--strict-mcp-config`, so the user's own connections (Executor) keep
//!   whatever attachment the launch policy gives them. A dynamic `luna` entry
//!   wins over a user-scope server of the same name (2.1.286 init probe).
//! - **Secret.** The bearer token lives in the child environment only. On Unix
//!   a `headersHelper` resolves it in the helper's shell, so the CLI's
//!   `mcp_status` echo of the config carries no credential (probe-verified);
//!   elsewhere the CLI expands `${LUNA_MCP_BEARER_TOKEN}` in the header.
//! - **Evidence.** Claude sends no thread or turn id over MCP; it sends
//!   `_meta["claudecode/toolUseId"]`, the model's `tool_use` id. A call is
//!   accepted only when that id was seen on stdout as a ROOT `tool_use` named
//!   `mcp__luna__<same tool>` in the current turn (from `system:init` to
//!   `result`). A call that races ahead of its stdout frame is parked for a
//!   short window; a subagent's (tagged) tool use is refused like a Codex
//!   sub-thread. Turn ids are minted here and announced as
//!   [`AgentEvent::ProviderTurnStarted`] so the engine's turn fence can claim
//!   the same call. The shared [`luna_mcp::dispatch`] fence (attached tool,
//!   call id claimed once per turn, interrupt) and the engine executor (scopes,
//!   exact one-use approval, receipts) then run unchanged.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use luna_proto::{AgentEvent, LunaAgentRuntimeContract};

use super::wire::{ControlRequestFrame, Frame, ObservedMcpStatus};
use crate::LunaToolExecutor;
use crate::codex::LunaCallIds;
use crate::codex::luna_mcp::{
    self, LunaMcpCall, LunaMcpEndpoint, LunaToolReply, LunaToolRequest, LunaToolScope,
};

/// Claude's MCP tool naming: `mcp__<server>__<tool>`.
const TOOL_PREFIX: &str = "mcp__luna__";
/// `_meta` key 2.1.286 sets on every MCP `tools/call` it makes for a tool use.
const TOOL_USE_META: &str = "claudecode/toolUseId";
/// How long a call may wait for its `tool_use` frame to be read from stdout.
const PARK_WINDOW: Duration = Duration::from_secs(5);
const MAX_PARKED: usize = 16;
const MAX_OBSERVED_PER_TURN: usize = 4_096;
const NOT_ATTACHED: &str = "LUNA tool is not attached to this task or turn";
const UNMATCHED: &str = "LUNA tool call does not match a tool use in this Claude turn";

/// The CLI flags and child environment that attach one endpoint.
pub(crate) struct ClaudeLunaAttachment {
    pub args: Vec<String>,
    pub env: (&'static str, String),
}

struct Turn {
    id: String,
    /// Root `tool_use` id → LUNA tool name (prefix stripped).
    root: HashMap<String, String>,
    /// Tool uses made inside a native subagent (tagged frames).
    foreign: HashSet<String>,
}

struct Parked {
    call: LunaMcpCall,
    tool_use_id: String,
    deadline: Instant,
}

enum Verdict {
    Dispatch,
    Park,
    Reject(&'static str),
}

/// Session-loop state for one Claude process's LUNA tools.
pub(crate) struct ClaudeLunaBridge {
    contract: LunaAgentRuntimeContract,
    executor: LunaToolExecutor,
    interrupt: CancellationToken,
    session_id: Option<String>,
    turn: Option<Turn>,
    call_ids: LunaCallIds,
    parked: Vec<Parked>,
}

impl ClaudeLunaBridge {
    /// Bind the shared endpoint before the spawn. The endpoint is returned
    /// separately: the session loop polls it, and dropping it retires the
    /// token with the process.
    pub(crate) async fn bind(
        contract: LunaAgentRuntimeContract,
        executor: LunaToolExecutor,
        interrupt: CancellationToken,
    ) -> std::io::Result<(LunaMcpEndpoint, ClaudeLunaAttachment, Self)> {
        let endpoint = LunaMcpEndpoint::bind(&contract.tools).await?;
        let attachment = ClaudeLunaAttachment {
            args: cli_args(endpoint.url(), &contract),
            env: (luna_mcp::TOKEN_ENV, endpoint.token().to_owned()),
        };
        Ok((
            endpoint,
            attachment,
            Self {
                contract,
                executor,
                interrupt,
                session_id: None,
                turn: None,
                call_ids: LunaCallIds::default(),
                parked: Vec::new(),
            },
        ))
    }

    /// Track the native turn and tool-use evidence from one stdout frame,
    /// before normalization. Returns the turn announcement to forward AFTER
    /// the frame's own events (so `SessionStarted` precedes it).
    pub(crate) fn observe(&mut self, frame: &Frame) -> Option<AgentEvent> {
        match frame {
            Frame::System(system) if system.subtype == "init" => {
                if !system.session_id.is_empty()
                    && self.session_id.as_deref() != Some(system.session_id.as_str())
                {
                    self.end_turn();
                    self.session_id = Some(system.session_id.clone());
                }
                // A managed policy can refuse dynamic servers; then the model
                // has no LUNA tools even though the contract attached them.
                if let Some(observation) = &system.integration_observation
                    && !observation.mcp_servers.iter().any(|server| {
                        server.name == luna_mcp::SERVER_NAME
                            && server.status == ObservedMcpStatus::Connected
                    })
                {
                    tracing::warn!(
                        target: "luna_harness::claude",
                        "Claude reports the luna MCP server as not connected; LUNA tools are unavailable in this session"
                    );
                }
                // A re-emitted init inside an open turn is not a new turn.
                if self.turn.is_some() {
                    return None;
                }
                let thread_id = self.session_id.clone()?;
                let turn_id = format!("claude-turn-{}", uuid::Uuid::new_v4().simple());
                self.turn = Some(Turn {
                    id: turn_id.clone(),
                    root: HashMap::new(),
                    foreign: HashSet::new(),
                });
                Some(AgentEvent::ProviderTurnStarted { thread_id, turn_id })
            }
            Frame::Assistant(message) => {
                let turn = self.turn.as_mut()?;
                let root = message.parent_tool_use_id.is_none();
                for block in message.message.blocks() {
                    let Some(tool) = (block.kind == "tool_use")
                        .then(|| block.name.strip_prefix(TOOL_PREFIX))
                        .flatten()
                    else {
                        continue;
                    };
                    if block.id.is_empty()
                        || turn.root.len() + turn.foreign.len() >= MAX_OBSERVED_PER_TURN
                    {
                        continue;
                    }
                    if root {
                        turn.root.insert(block.id.clone(), tool.to_owned());
                    } else {
                        turn.foreign.insert(block.id.clone());
                    }
                }
                self.release_parked();
                None
            }
            Frame::Result(_) => {
                self.end_turn();
                None
            }
            _ => None,
        }
    }

    /// One `tools/call` from the endpoint.
    pub(crate) fn on_call(&mut self, call: LunaMcpCall) {
        let tool_use_id = call
            .meta
            .get(TOOL_USE_META)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match self.verdict(&tool_use_id, &call.name) {
            Verdict::Dispatch => self.dispatch(call, &tool_use_id),
            Verdict::Reject(message) => reject(call, message),
            Verdict::Park if self.parked.len() < MAX_PARKED => self.parked.push(Parked {
                call,
                tool_use_id,
                deadline: Instant::now() + PARK_WINDOW,
            }),
            Verdict::Park => reject(call, UNMATCHED),
        }
    }

    /// Earliest parked deadline, for the session loop's timer branch.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.parked.iter().map(|parked| parked.deadline).min()
    }

    /// Refuse parked calls whose `tool_use` never appeared in time.
    pub(crate) fn expire(&mut self) {
        let now = Instant::now();
        let (expired, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.parked)
            .into_iter()
            .partition(|parked| parked.deadline <= now);
        self.parked = waiting;
        for parked in expired {
            reject(parked.call, UNMATCHED);
        }
    }

    /// Claude's own permission prompt for an attached LUNA tool. The CLI is
    /// already given `--allowedTools` for these; this keeps a policy-mode
    /// prompt from asking twice. The engine still enforces scopes and asks
    /// for exact approval before any mutation.
    pub(crate) fn preapproved(&self, request: &ControlRequestFrame) -> bool {
        request.request.subtype == "can_use_tool"
            && request
                .request
                .tool_name
                .strip_prefix(TOOL_PREFIX)
                .is_some_and(|name| self.contract.tools.iter().any(|tool| tool.name == name))
    }

    fn verdict(&self, tool_use_id: &str, name: &str) -> Verdict {
        // Between turns (or before the first `system:init` is read) a call
        // waits for the turn and its tool use; it is never run without them.
        let Some(turn) = self.turn.as_ref() else {
            return Verdict::Park;
        };
        if tool_use_id.is_empty() {
            // The shared fence answers with its "call ID is missing" error.
            return Verdict::Dispatch;
        }
        if turn.foreign.contains(tool_use_id) {
            return Verdict::Reject(NOT_ATTACHED);
        }
        match turn.root.get(tool_use_id) {
            Some(observed) if observed == name => Verdict::Dispatch,
            Some(_) => Verdict::Reject(UNMATCHED),
            None => Verdict::Park,
        }
    }

    fn dispatch(&mut self, call: LunaMcpCall, tool_use_id: &str) {
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
                call_id: tool_use_id,
                foreign_namespace: false,
                name: &name,
                arguments,
            },
            LunaToolScope {
                thread_id,
                active_turn_id: Some(turn.id.as_str()),
                runtime_contract: Some(&self.contract),
                invoke_luna_tool: Some(&self.executor),
                call_ids: &mut self.call_ids,
                interrupt: &self.interrupt,
            },
            LunaToolReply::Mcp(reply),
        );
    }

    fn release_parked(&mut self) {
        for parked in std::mem::take(&mut self.parked) {
            if parked.call.reply.is_closed() {
                continue;
            }
            match self.verdict(&parked.tool_use_id, &parked.call.name) {
                Verdict::Park => self.parked.push(parked),
                Verdict::Dispatch => {
                    let tool_use_id = parked.tool_use_id;
                    self.dispatch(parked.call, &tool_use_id);
                }
                Verdict::Reject(message) => reject(parked.call, message),
            }
        }
    }

    fn end_turn(&mut self) {
        self.turn = None;
        for parked in std::mem::take(&mut self.parked) {
            reject(parked.call, NOT_ATTACHED);
        }
    }
}

fn reject(call: LunaMcpCall, message: &str) {
    let _ = call.reply.send((false, message.to_owned()));
}

/// `--mcp-config` names only the `luna` server; `--allowedTools` lists exactly
/// the attached tools. The token never appears in argv.
fn cli_args(url: &str, contract: &LunaAgentRuntimeContract) -> Vec<String> {
    let mut server = json!({"type": "http", "url": url});
    if cfg!(unix) {
        server["headersHelper"] = Value::String(format!(
            "printf '{{\"Authorization\":\"Bearer %s\"}}' \"${}\"",
            luna_mcp::TOKEN_ENV
        ));
    } else {
        server["headers"] = json!({
            "Authorization": format!("Bearer ${{{}}}", luna_mcp::TOKEN_ENV)
        });
    }
    let mut servers = serde_json::Map::new();
    servers.insert(luna_mcp::SERVER_NAME.into(), server);
    let config = json!({ "mcpServers": servers });
    let allowed = contract
        .tools
        .iter()
        .map(|tool| format!("{TOOL_PREFIX}{}", tool.name))
        .collect::<Vec<_>>()
        .join(",");
    vec![
        "--mcp-config".into(),
        config.to_string(),
        "--allowedTools".into(),
        allowed,
    ]
}

/// Next endpoint call, or pending forever without an endpoint.
pub(crate) async fn next_call(endpoint: &mut Option<LunaMcpEndpoint>) -> Option<LunaMcpCall> {
    LunaMcpEndpoint::next_call(endpoint).await
}

/// Sleep until a parked call's deadline, or forever when nothing is parked.
pub(crate) async fn parked_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
