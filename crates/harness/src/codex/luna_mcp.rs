//! Engine-executed LUNA tools on the Codex wire.
//!
//! Every ordinary app-server spawn gets a private, loopback-only MCP endpoint
//! named `luna`. Unlike `thread/start` dynamic tools, Codex connects to it on
//! every process start, so resumed and pre-tool threads see the current tool
//! set. The endpoint only frames requests: each `tools/call` goes through the
//! session loop's exact thread/turn/call-ID fence and then the engine executor,
//! the same path as a legacy `item/tool/call`.
//!
//! The bearer token lives only in this process and the child's environment
//! (Codex's default shell policy strips `*TOKEN*` variables from model
//! commands). The endpoint shuts down with the session that owns it.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::LunaCallIds;
use crate::jsonrpc::RpcClient;
use crate::{HarnessError, LunaToolExecutor, LunaToolInvocation, LunaToolInvocationContext};

/// Codex server name. It keeps existing `ToolCall::Mcp { server: "luna" }`
/// presentation and activity mapping.
pub(crate) const SERVER_NAME: &str = "luna";
/// Child environment variable read by Codex `bearer_token_env_var`.
pub(crate) const TOKEN_ENV: &str = "LUNA_MCP_BEARER_TOKEN";
/// A parked LUNA approval waits for the user, not a transport deadline. The
/// engine bounds the same card by `LUNA_TOOL_APPROVAL_WAIT_MS`.
pub(crate) const TOOL_TIMEOUT_SEC: u64 = 86_400;
const MAX_BODY_BYTES: usize = 1 << 20;
const MAX_CONNECTIONS: usize = 32;
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const NOT_ATTACHED: &str = "LUNA tool is not attached to this task or turn";

/// One `tools/call` handed to the session loop. `meta` is Codex's `_meta`.
pub(crate) struct LunaMcpCall {
    pub name: String,
    pub arguments: Value,
    pub meta: Value,
    pub reply: oneshot::Sender<(bool, String)>,
}

pub(crate) struct LunaMcpEndpoint {
    url: String,
    token: String,
    calls: mpsc::Receiver<LunaMcpCall>,
    shutdown: CancellationToken,
}

impl Drop for LunaMcpEndpoint {
    fn drop(&mut self) {
        // Retires the token with its session: open connections and parked
        // calls are dropped, and later connections are refused.
        self.shutdown.cancel();
    }
}

struct ServerState {
    token: String,
    authority: String,
    tools: Value,
    calls: mpsc::Sender<LunaMcpCall>,
    in_flight: Mutex<HashMap<String, CancellationToken>>,
    shutdown: CancellationToken,
}

impl LunaMcpEndpoint {
    pub(crate) async fn bind(tools: &[luna_proto::RuntimeToolDefinition]) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        // Two v4 UUIDs: 244 random bits from the OS generator.
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let (calls_tx, calls) = mpsc::channel(32);
        let shutdown = CancellationToken::new();
        let state = Arc::new(ServerState {
            token: token.clone(),
            authority: format!("127.0.0.1:{port}"),
            tools: tool_list(tools),
            calls: calls_tx,
            in_flight: Mutex::new(HashMap::new()),
            shutdown: shutdown.clone(),
        });
        tokio::spawn(serve(listener, state));
        Ok(Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            token,
            calls,
            shutdown,
        })
    }

    /// `-c` overrides for this spawn only; the user's Codex config is never
    /// edited. Codex's own approval prompt is pre-approved for this server
    /// alone: reads are read-only, and both writes require the exact local
    /// LUNA approval in the engine.
    pub(crate) fn codex_args(&self) -> Vec<String> {
        let server = format!("mcp_servers.{SERVER_NAME}");
        [
            format!("{server}.url=\"{}\"", self.url),
            format!("{server}.bearer_token_env_var=\"{TOKEN_ENV}\""),
            format!("{server}.enabled=true"),
            format!("{server}.default_tools_approval_mode=\"approve\""),
            format!("{server}.tool_timeout_sec={TOOL_TIMEOUT_SEC}"),
            format!("{server}.startup_timeout_sec=20"),
        ]
        .into_iter()
        .flat_map(|value| ["-c".to_owned(), value])
        .collect()
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Loopback URL of this endpoint (the Claude adapter's `--mcp-config`).
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// Next MCP tool call, or pending forever when no endpoint is attached.
    pub(crate) async fn next_call(endpoint: &mut Option<Self>) -> Option<LunaMcpCall> {
        match endpoint {
            Some(endpoint) => endpoint.calls.recv().await,
            None => std::future::pending().await,
        }
    }
}

fn tool_list(tools: &[luna_proto::RuntimeToolDefinition]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let read_only = tool.idempotency == "read-only";
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "annotations": {
                        "readOnlyHint": read_only,
                        "destructiveHint": false,
                        "idempotentHint": true,
                        "openWorldHint": false,
                    },
                })
            })
            .collect(),
    )
}

async fn serve(listener: tokio::net::TcpListener, state: Arc<ServerState>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let accepted = tokio::select! {
            _ = state.shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((socket, peer)) = accepted else {
            // Transient accept failures (for example EMFILE) must not spin.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let _permit = permit;
            let shutdown = state.shutdown.clone();
            let service = hyper::service::service_fn(move |request| {
                let state = Arc::clone(&state);
                async move { Ok::<_, Infallible>(handle(request, &state).await) }
            });
            let connection = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(socket), service);
            tokio::select! {
                _ = shutdown.cancelled() => {}
                _ = connection => {}
            }
        });
    }
}

fn plain(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

fn json_response(value: Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn rpc_result(id: Value, result: Value) -> Response<Full<Bytes>> {
    json_response(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response<Full<Bytes>> {
    json_response(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn tool_result(success: bool, text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": !success})
}

/// Constant-time comparison for the bearer secret.
fn same_secret(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (left, right)| acc | (left ^ right))
            == 0
}

fn authorized(request: &Request<Incoming>, token: &str) -> bool {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| same_secret(presented.trim().as_bytes(), token.as_bytes()))
}

async fn handle(request: Request<Incoming>, state: &ServerState) -> Response<Full<Bytes>> {
    if request.uri().path() != "/mcp" {
        return plain(StatusCode::NOT_FOUND);
    }
    // Browsers always send Origin; a rebinding hostname fails the Host check.
    if request.headers().contains_key(header::ORIGIN)
        || request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            != Some(state.authority.as_str())
    {
        return plain(StatusCode::FORBIDDEN);
    }
    if !authorized(&request, &state.token) {
        let mut response = plain(StatusCode::UNAUTHORIZED);
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    if request.method() != Method::POST {
        // No server-initiated SSE stream and no session to delete.
        let mut response = plain(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let body = match Limited::new(request.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE),
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "Parse error");
    };
    // MCP 2025-06-18 has no batches; reject them rather than half-serve one.
    let Some(object) = message.as_object() else {
        return rpc_error(Value::Null, -32600, "Invalid Request");
    };
    let method = object.get("method").and_then(Value::as_str);
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = object.get("id").cloned() else {
        if method == Some("notifications/cancelled")
            && let Some(request_id) = params.get("requestId")
            && let Some(cancel) = state
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&request_id.to_string())
        {
            cancel.cancel();
        }
        // Notifications and client responses carry no reply.
        return plain(StatusCode::ACCEPTED);
    };
    match method {
        Some("initialize") => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|version| PROTOCOL_VERSIONS.contains(version))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            rpc_result(
                id,
                json!({
                    "protocolVersion": requested,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": SERVER_NAME, "title": "LUNA", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
        }
        Some("ping") => rpc_result(id, json!({})),
        Some("tools/list") => rpc_result(id, json!({"tools": state.tools})),
        Some("tools/call") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(id, -32602, "Invalid params");
            };
            let cancel = CancellationToken::new();
            let key = id.to_string();
            state
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key.clone(), cancel.clone());
            let result = call_tool(state, name, &params, &cancel).await;
            state
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
            rpc_result(id, result)
        }
        Some(_) => rpc_error(id, -32601, "Method not found"),
        None => rpc_error(id, -32600, "Invalid Request"),
    }
}

async fn call_tool(
    state: &ServerState,
    name: &str,
    params: &Value,
    cancel: &CancellationToken,
) -> Value {
    let (reply, answer) = oneshot::channel();
    let call = LunaMcpCall {
        name: name.to_owned(),
        arguments: params
            .get("arguments")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!({})),
        meta: params.get("_meta").cloned().unwrap_or(Value::Null),
        reply,
    };
    if state.calls.send(call).await.is_err() {
        return tool_result(false, NOT_ATTACHED.into());
    }
    // Dropping `answer` (cancel, disconnect, shutdown) abandons the call: the
    // dispatcher stops the executor and the engine retires any parked card.
    tokio::select! {
        _ = cancel.cancelled() => tool_result(false, "LUNA tool call cancelled".into()),
        answer = answer => match answer {
            Ok((success, text)) => tool_result(success, text),
            Err(_) => tool_result(false, NOT_ATTACHED.into()),
        },
    }
}

/// Provider-native identity of one LUNA tool request as read from the wire.
pub(crate) struct LunaToolRequest<'a> {
    pub thread_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub call_id: &'a str,
    pub foreign_namespace: bool,
    pub name: &'a str,
    pub arguments: Value,
}

impl<'a> LunaToolRequest<'a> {
    /// Codex 0.159 `_meta`: `threadId`, `callId`, and the turn's
    /// `x-codex-turn-metadata.turn_id`. Missing fields fail the fence.
    pub(crate) fn from_mcp(name: &'a str, arguments: &Value, meta: &'a Value) -> Self {
        Self {
            thread_id: meta.get("threadId").and_then(Value::as_str),
            turn_id: meta
                .get("x-codex-turn-metadata")
                .and_then(|turn| turn.get("turn_id"))
                .and_then(Value::as_str),
            call_id: meta.get("callId").and_then(Value::as_str).unwrap_or(""),
            foreign_namespace: false,
            name,
            arguments: arguments.clone(),
        }
    }

    /// Legacy `item/tool/call` from a thread started with `dynamicTools`.
    pub(crate) fn from_dynamic(params: &'a Value) -> Self {
        Self {
            thread_id: params.get("threadId").and_then(Value::as_str),
            turn_id: params.get("turnId").and_then(Value::as_str),
            call_id: params.get("callId").and_then(Value::as_str).unwrap_or(""),
            foreign_namespace: !params.get("namespace").is_none_or(Value::is_null),
            name: params.get("tool").and_then(Value::as_str).unwrap_or(""),
            arguments: params.get("arguments").cloned().unwrap_or(Value::Null),
        }
    }
}

pub(crate) enum LunaToolReply {
    Dynamic { client: RpcClient, id: Value },
    Mcp(oneshot::Sender<(bool, String)>),
}

impl LunaToolReply {
    fn send(self, success: bool, text: String) {
        match self {
            Self::Dynamic { client, id } => client.respond(
                &id,
                json!({"success": success, "contentItems": [{"type": "inputText", "text": text}]}),
            ),
            Self::Mcp(reply) => {
                let _ = reply.send((success, text));
            }
        }
    }

    async fn abandoned(&mut self) {
        match self {
            Self::Dynamic { .. } => std::future::pending().await,
            Self::Mcp(reply) => reply.closed().await,
        }
    }
}

/// The session's authority for one request: its root thread, active turn,
/// attached contract, engine executor and per-turn call-ID ledger.
pub(crate) struct LunaToolScope<'a> {
    pub thread_id: &'a str,
    pub active_turn_id: Option<&'a str>,
    pub runtime_contract: Option<&'a luna_proto::LunaAgentRuntimeContract>,
    pub invoke_luna_tool: Option<&'a LunaToolExecutor>,
    pub call_ids: &'a mut LunaCallIds,
    pub interrupt: &'a CancellationToken,
}

/// The one adapter fence for both transports: exact root thread and active
/// turn, an attached tool, and a call ID claimed once per turn. Only then
/// does the engine executor (with its own claim and approval) run.
pub(crate) fn dispatch(
    request: LunaToolRequest<'_>,
    scope: LunaToolScope<'_>,
    reply: LunaToolReply,
) {
    let matching_context = scope.active_turn_id.is_some()
        && request.thread_id == Some(scope.thread_id)
        && request.turn_id == scope.active_turn_id
        && !request.foreign_namespace
        && !scope.interrupt.is_cancelled();
    let attached = scope
        .runtime_contract
        .is_some_and(|contract| contract.tools.iter().any(|tool| tool.name == request.name));
    let Some(executor) = scope
        .invoke_luna_tool
        .filter(|_| matching_context && attached)
    else {
        reply.send(false, NOT_ATTACHED.into());
        return;
    };
    let Some(turn_id) = scope
        .active_turn_id
        .filter(|turn_id| scope.call_ids.claim(turn_id, request.call_id))
    else {
        reply.send(
            false,
            "LUNA tool call ID is missing, repeated, or over limit".into(),
        );
        return;
    };
    let executor = Arc::clone(executor);
    let interrupt = scope.interrupt.clone();
    let invocation = LunaToolInvocation {
        name: request.name.to_owned(),
        arguments: request.arguments,
        context: LunaToolInvocationContext::new(scope.thread_id, turn_id, request.call_id),
    };
    tokio::spawn(async move {
        let mut reply = reply;
        let outcome = tokio::select! {
            biased;
            _ = interrupt.cancelled() => Err(HarnessError::Protocol("LUNA tool call cancelled".into())),
            // The provider gave up (MCP cancel or disconnect): drop the
            // executor so a parked approval card is retired, not left live.
            _ = reply.abandoned() => return,
            result = executor(invocation) => result,
        };
        let (success, text) = match outcome {
            Ok(value) => (true, value.to_string()),
            Err(error) => (false, error.to_string()),
        };
        reply.send(success, text);
    });
}
