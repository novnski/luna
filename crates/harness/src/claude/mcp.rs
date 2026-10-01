//! Claude MCP control plane (CC-11): effective server state, provider-owned
//! enable/disable, and MCP Apps `ui://` resource reads.
//!
//! Everything goes through a short-lived *control-only* CLI process: the same
//! resolved executable and inherited environment as a real run, the selected
//! project as cwd, and the run's settings-shaping launch flags minus any
//! permission-mode or bypass flag. Only `control_request` lines are written —
//! never a user message — so no model turn or tool call can happen.
//!
//! Live-verified against 2.1.286 in an isolated config (cc11 probes):
//! - `mcp_status` → `{mcpServers:[{name,status,scope,source,config,serverInfo?,
//!   error?,tools?}]}`; servers start `pending` (non-blocking MCP startup).
//! - `--restricted` loads no user/project/local servers; an explicit
//!   `--mcp-config` (scope `dynamic`) still loads.
//! - `mcp_toggle` persists in Claude's own config for that cwd.
//! - `mcp_read_resource` accepts only `ui://` and strips `com.anthropic/` meta.
//!
//! Provider `config` values (URLs, headers, commands, args, env) are never
//! retained; error text is bounded with paths and URLs redacted.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use luna_proto::{PermissionMode, ProviderMcpAppResourceView, ProviderMcpStatus, ProviderMcpTool};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};

use super::wire::SystemFrame;
use crate::HarnessError;
use crate::mcp_control::{
    McpControlOp, McpControlReply, McpControlRequest, McpLaunchReadback, McpWriteOutcome,
    ProviderMcpServer, SessionMcpObservation, SessionMcpServer,
};
use crate::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const MAX_SERVERS: usize = 256;
const MAX_SESSION_SERVERS: usize = 128;
const MAX_TOOLS: usize = 512;
const MAX_NAME_BYTES: usize = 256;
const MAX_URI_BYTES: usize = 2_048;
const MAX_ERROR_CHARS: usize = 400;
const MAX_PREVIEW_CHARS: usize = 4_096;
const MAX_CSP_ENTRIES: usize = 32;
const MAX_SESSIONS: usize = 256;
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const SETTLE_DEADLINE: Duration = Duration::from_secs(8);
const SETTLE_POLL: Duration = Duration::from_millis(300);
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// Wording 2.1.x uses for a control subtype the CLI does not implement.
const UNSUPPORTED_SUBTYPE: &str = "Unsupported control request subtype";
pub(crate) const APP_RENDER_UNAVAILABLE: &str = "LUNA does not render MCP App views yet: untrusted app HTML needs an isolated host that enforces the declared CSP and binds app tool calls to the owning chat, and the general browser pane is not that host. The source is shown as inert text for inspection only.";

// ---------------------------------------------------------------------------
// Redaction and bounded decoding
// ---------------------------------------------------------------------------

fn clean_word(value: Option<&Value>, allowed: &[&str]) -> Option<String> {
    let word = value?.as_str()?;
    allowed.contains(&word).then(|| word.to_owned())
}

/// A provider-chosen name: trimmed, printable, bounded. Names are identities,
/// so anything ambiguous is rejected rather than rewritten.
fn clean_name(value: Option<&Value>) -> Option<String> {
    let name = value?.as_str()?;
    if name.is_empty()
        || name.trim() != name
        || name.len() > MAX_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return None;
    }
    Some(name.to_owned())
}

fn clean_short(value: Option<&Value>, limit: usize) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Bounded provider text with absolute paths and URLs replaced. Provider
/// errors routinely echo a configured command path or server URL.
pub(crate) fn redact_provider_text(text: &str, limit: usize) -> String {
    let flattened: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out = Vec::new();
    for token in flattened.split_whitespace() {
        let core = token.trim_matches(|c: char| "'\"`()[]{},;<>".contains(c));
        let replacement = if core.contains("://") {
            Some("[url]")
        } else if core.starts_with('/')
            || core.starts_with("~/")
            || core.starts_with("\\\\")
            || (core.len() > 2 && core.as_bytes()[1] == b':' && core.as_bytes()[2] == b'\\')
        {
            Some("[path]")
        } else {
            None
        };
        out.push(replacement.map_or_else(|| token.to_owned(), str::to_owned));
    }
    let joined = out.join(" ");
    if joined.chars().count() > limit {
        let mut bounded: String = joined.chars().take(limit).collect();
        bounded.push('…');
        bounded
    } else {
        joined
    }
}

fn status_word(word: Option<&str>) -> ProviderMcpStatus {
    match word {
        Some("connected") => ProviderMcpStatus::Connected,
        Some("failed") => ProviderMcpStatus::Failed,
        Some("needs-auth") => ProviderMcpStatus::NeedsAuth,
        Some("pending") => ProviderMcpStatus::Pending,
        Some("disabled") => ProviderMcpStatus::Disabled,
        _ => ProviderMcpStatus::Unknown,
    }
}

const SCOPES: &[&str] = &[
    "user",
    "project",
    "local",
    "dynamic",
    "managed",
    "enterprise",
    "claudeai",
    "plugin",
    "sdk",
];
const TRANSPORTS: &[&str] = &[
    "stdio",
    "http",
    "sse",
    "sse-ide",
    "ws",
    "ws-ide",
    "sdk",
    "claudeai-proxy",
];

fn valid_ui_uri(uri: &str) -> bool {
    uri.starts_with("ui://")
        && uri.len() > "ui://".len()
        && uri.len() <= MAX_URI_BYTES
        && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn decode_tool(value: &Value) -> Option<ProviderMcpTool> {
    let object = value.as_object()?;
    let name = clean_name(object.get("name"))?;
    let meta = object.get("_meta").and_then(Value::as_object);
    let ui = meta
        .and_then(|meta| meta.get("ui"))
        .and_then(Value::as_object);
    let ui_resource_uri = ui
        .and_then(|ui| ui.get("resourceUri"))
        .or_else(|| meta.and_then(|meta| meta.get("ui/resourceUri")))
        .and_then(Value::as_str)
        .filter(|uri| valid_ui_uri(uri))
        .map(str::to_owned);
    let ui_visibility = ui
        .and_then(|ui| ui.get("visibility"))
        .and_then(Value::as_array)
        .map(|words| {
            words
                .iter()
                .filter_map(|word| clean_word(Some(word), &["model", "app"]))
                .take(4)
                .collect()
        })
        .unwrap_or_default();
    Some(ProviderMcpTool {
        name,
        ui_resource_uri,
        ui_visibility,
    })
}

fn decode_server(value: &Value) -> Option<ProviderMcpServer> {
    let object = value.as_object()?;
    let name = clean_name(object.get("name"))?;
    let status = status_word(object.get("status").and_then(Value::as_str));
    let config = object.get("config").and_then(Value::as_object);
    let transport = clean_word(config.and_then(|c| c.get("type")), TRANSPORTS).or_else(|| {
        // The CLI treats a typeless config with a command as stdio.
        config
            .filter(|c| c.get("type").is_none() && c.contains_key("command"))
            .map(|_| "stdio".to_owned())
    });
    let server_version = object
        .get("serverInfo")
        .and_then(Value::as_object)
        .and_then(|info| {
            let name = clean_short(info.get("name"), 96)?;
            let version = clean_short(info.get("version"), 48).unwrap_or_default();
            Some(format!("{name} {version}").trim().to_owned())
        });
    let error = object
        .get("error")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| redact_provider_text(text, MAX_ERROR_CHARS));
    let error_code = object
        .get("error_code")
        .and_then(Value::as_str)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
        })
        .map(str::to_owned);
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .take(MAX_TOOLS)
                .filter_map(decode_tool)
                .collect()
        })
        .unwrap_or_default();
    let scope = clean_word(object.get("scope"), SCOPES);
    Some(ProviderMcpServer {
        name,
        status,
        toggleable: toggleable_scope(scope.as_deref()),
        scope,
        transport,
        server_version,
        error,
        error_code,
        tools,
    })
}

/// Decode an `mcp_status` success payload. A missing/oversized list is a
/// malformed readback, never "zero servers".
pub(crate) fn decode_status(payload: &Value) -> Result<Vec<ProviderMcpServer>, HarnessError> {
    let rows = payload
        .get("mcpServers")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= MAX_SERVERS)
        .ok_or_else(|| {
            HarnessError::Protocol("Claude returned a malformed MCP status readback".into())
        })?;
    let mut servers: Vec<ProviderMcpServer> = Vec::new();
    for row in rows {
        let Some(server) = decode_server(row) else {
            continue;
        };
        // One effective row per name, as Claude itself reports precedence.
        if servers.iter().any(|existing| existing.name == server.name) {
            continue;
        }
        servers.push(server);
    }
    Ok(servers)
}

// ---------------------------------------------------------------------------
// Session evidence (the run's own `system:init`)
// ---------------------------------------------------------------------------

/// Bounded in-memory map of native session id → latest MCP start report.
#[derive(Default)]
pub(crate) struct SessionMcpRegistry {
    observations: Mutex<VecDeque<SessionMcpObservation>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn bounded_identity(text: &str, limit: usize) -> bool {
    !text.is_empty()
        && text.trim() == text
        && text.len() <= limit
        && !text.chars().any(char::is_control)
}

/// Parse an init frame's `mcp_servers` + `capabilities`. Any malformed row
/// discards the whole observation: a partial list would turn "unreadable"
/// into a false "not attached".
fn session_observation(frame: &SystemFrame) -> Option<SessionMcpObservation> {
    if frame.subtype != "init"
        || !bounded_identity(&frame.session_id, 256)
        || !bounded_identity(&frame.cwd, 2_048)
    {
        return None;
    }
    let rows = frame.mcp_servers.as_deref()?.as_array()?;
    if rows.len() > MAX_SESSION_SERVERS {
        return None;
    }
    let mut servers = Vec::with_capacity(rows.len());
    for row in rows {
        let object = row.as_object()?;
        let name = clean_name(object.get("name"))?;
        let status = status_word(Some(object.get("status")?.as_str()?));
        if servers.iter().any(|s: &SessionMcpServer| s.name == name) {
            return None;
        }
        servers.push(SessionMcpServer {
            name,
            status,
            source: clean_word(object.get("source"), SCOPES),
        });
    }
    let capabilities: Option<Vec<&str>> = frame
        .capabilities
        .as_deref()
        .and_then(Value::as_array)
        .map(|words| words.iter().filter_map(Value::as_str).collect());
    Some(SessionMcpObservation {
        session_id: frame.session_id.clone(),
        cwd: frame.cwd.clone(),
        observed_at_ms: now_ms(),
        servers,
        capabilities_reported: capabilities.is_some(),
        app_read_capability: capabilities
            .as_ref()
            .is_some_and(|words| words.contains(&"mcp_read_resource_v1")),
        app_tool_meta_capability: capabilities
            .as_ref()
            .is_some_and(|words| words.contains(&"mcp_tool_ui_meta_v1")),
    })
}

impl SessionMcpRegistry {
    /// Record a run's init frame (the latest one per session wins).
    pub(crate) fn observe(&self, frame: &SystemFrame) {
        let Some(observation) = session_observation(frame) else {
            return;
        };
        let Ok(mut observations) = self.observations.lock() else {
            return;
        };
        observations.retain(|existing| existing.session_id != observation.session_id);
        if observations.len() >= MAX_SESSIONS {
            observations.pop_front();
        }
        observations.push_back(observation);
    }

    pub(crate) fn get(&self, session_id: &str) -> Option<SessionMcpObservation> {
        self.observations
            .lock()
            .ok()?
            .iter()
            .find(|observation| observation.session_id == session_id)
            .cloned()
    }
}

// ---------------------------------------------------------------------------
// Control-only provider process
// ---------------------------------------------------------------------------

/// The run's settings-shaping flags for `mode` with every permission-mode and
/// bypass flag removed: the admin process must see the same configuration as
/// the real run without ever carrying a permission grant.
pub(crate) fn admin_policy_args(mode: PermissionMode) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut args = super::claude_permission_args(mode).iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--permission-mode" => {
                args.next();
            }
            "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions" => {}
            other => out.push(other),
        }
    }
    out
}

enum Answer {
    Success(Value),
    Error(String),
}

enum Failure {
    /// No correlated answer before the deadline.
    Timeout,
    /// The process ended (or stdout closed) before answering.
    Closed,
    Io(std::io::Error),
}

impl Failure {
    fn into_error(self, what: &str) -> HarnessError {
        match self {
            Self::Timeout => {
                HarnessError::Protocol(format!("Claude did not answer {what} in time"))
            }
            Self::Closed => {
                HarnessError::Protocol(format!("Claude exited before answering {what}"))
            }
            Self::Io(error) => HarnessError::Io(error),
        }
    }
}

struct AdminProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    kill_grace: Duration,
}

impl AdminProcess {
    async fn spawn(
        exe: &Path,
        cwd: &Path,
        policy_args: &[&str],
        kill_grace: Duration,
    ) -> Result<Self, HarnessError> {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(HarnessError::Protocol(
                "MCP readback needs an existing absolute project folder".into(),
            ));
        }
        let mut cmd = Command::new(exe);
        crate::compose_child_path(&mut cmd, exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ]);
        cmd.args(policy_args);
        cmd.current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(error)
            }
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            crate::shutdown_child(&mut child, kill_grace).await;
            return Err(HarnessError::Protocol("claude child has no stdio".into()));
        };
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines: BufReader::new(stdout).lines(),
            next_id: 0,
            kill_grace,
        })
    }

    /// Write one control request and wait for its correlated response. Other
    /// stdout lines are ignored (no user message is ever written).
    async fn request(&mut self, body: Value, timeout: Duration) -> Result<Answer, Failure> {
        self.next_id += 1;
        let request_id = format!("luna-mcp-{}", self.next_id);
        let line = json!({"type": "control_request", "request_id": request_id, "request": body});
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(Failure::Closed);
        };
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(Failure::Io)?;
        stdin.flush().await.map_err(Failure::Io)?;
        let lines = &mut self.lines;
        let wait = async {
            while let Some(line) = lines.next_line().await.map_err(Failure::Io)? {
                let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                    continue;
                }
                let response = frame.get("response").cloned().unwrap_or(Value::Null);
                if response.get("request_id").and_then(Value::as_str) != Some(request_id.as_str()) {
                    continue;
                }
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    let message = response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Claude rejected the control request")
                        .to_owned();
                    return Ok(Answer::Error(message));
                }
                return Ok(Answer::Success(
                    response.get("response").cloned().unwrap_or(Value::Null),
                ));
            }
            Err(Failure::Closed)
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or(Err(Failure::Timeout))
    }

    async fn initialize(&mut self) -> Result<(), HarnessError> {
        match self
            .request(json!({"subtype": "initialize"}), INIT_TIMEOUT)
            .await
        {
            Ok(Answer::Success(_)) => Ok(()),
            Ok(Answer::Error(message)) => Err(HarnessError::Protocol(redact_provider_text(
                &message,
                MAX_ERROR_CHARS,
            ))),
            Err(failure) => Err(failure.into_error("initialize")),
        }
    }

    async fn status(&mut self) -> Result<Vec<ProviderMcpServer>, HarnessError> {
        match self
            .request(json!({"subtype": "mcp_status"}), REQUEST_TIMEOUT)
            .await
        {
            Ok(Answer::Success(payload)) => decode_status(&payload),
            Ok(Answer::Error(message)) => Err(HarnessError::Protocol(redact_provider_text(
                &message,
                MAX_ERROR_CHARS,
            ))),
            Err(failure) => Err(failure.into_error("mcp_status")),
        }
    }

    /// Poll `mcp_status` until `done` holds or the deadline passes. Returns
    /// the last readback and whether it settled.
    async fn status_until(
        &mut self,
        done: impl Fn(&[ProviderMcpServer]) -> bool,
    ) -> Result<(Vec<ProviderMcpServer>, bool), HarnessError> {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let servers = self.status().await?;
            if done(&servers) {
                return Ok((servers, true));
            }
            if Instant::now() + SETTLE_POLL >= deadline {
                return Ok((servers, false));
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// Side-effect-free capability check: a deliberately invalid read is
    /// refused by argument validation on CLIs that implement the control,
    /// and by the subtype dispatcher on CLIs that do not.
    async fn app_read_supported(&mut self) -> Option<bool> {
        match self
            .request(
                json!({"subtype": "mcp_read_resource", "serverName": "", "uri": ""}),
                REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(Answer::Error(message)) => Some(!message.contains(UNSUPPORTED_SUBTYPE)),
            Ok(Answer::Success(_)) | Err(_) => None,
        }
    }

    /// Close stdin (the CLI exits on EOF, after any pending config write),
    /// then make sure the child is gone.
    async fn finish(mut self) {
        drop(self.stdin.take());
        if tokio::time::timeout(EXIT_GRACE, self.child.wait())
            .await
            .is_err()
        {
            crate::shutdown_child(&mut self.child, self.kill_grace).await;
        }
    }
}

fn none_pending(servers: &[ProviderMcpServer]) -> bool {
    !servers
        .iter()
        .any(|server| server.status == ProviderMcpStatus::Pending)
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

pub(crate) async fn control(
    exe: PathBuf,
    kill_grace: Duration,
    request: McpControlRequest,
) -> Result<McpControlReply, HarnessError> {
    match request.op {
        McpControlOp::Status { modes } => {
            status_readbacks(&exe, &request.cwd, kill_grace, modes).await
        }
        McpControlOp::SetEnabled { server, enabled } => {
            set_enabled(&exe, &request.cwd, kill_grace, &server, enabled).await
        }
        McpControlOp::ReadAppResource { server, uri } => {
            read_app_resource(&exe, &request.cwd, kill_grace, &server, &uri).await
        }
    }
}

async fn one_readback(
    exe: &Path,
    cwd: &Path,
    kill_grace: Duration,
    args: Vec<&'static str>,
    modes: Vec<PermissionMode>,
) -> Result<McpLaunchReadback, HarnessError> {
    let mut process = AdminProcess::spawn(exe, cwd, &args, kill_grace).await?;
    let result = async {
        process.initialize().await?;
        let (servers, settled) = process.status_until(none_pending).await?;
        let declares_apps = servers.iter().any(|server| {
            server
                .tools
                .iter()
                .any(|tool| tool.ui_resource_uri.is_some())
        });
        let app_read_supported = if declares_apps {
            process.app_read_supported().await
        } else {
            None
        };
        Ok(McpLaunchReadback {
            restricted: args.contains(&"--restricted"),
            modes,
            settled,
            servers,
            app_read_supported,
        })
    }
    .await;
    process.finish().await;
    result
}

async fn status_readbacks(
    exe: &Path,
    cwd: &Path,
    kill_grace: Duration,
    modes: Vec<PermissionMode>,
) -> Result<McpControlReply, HarnessError> {
    // Group modes whose admin flags are identical: one provider process each.
    let mut groups: Vec<(Vec<&'static str>, Vec<PermissionMode>)> = Vec::new();
    for mode in modes {
        let args = admin_policy_args(mode);
        match groups.iter_mut().find(|(existing, _)| *existing == args) {
            Some((_, grouped)) if !grouped.contains(&mode) => grouped.push(mode),
            Some(_) => {}
            None => groups.push((args, vec![mode])),
        }
    }
    if groups.is_empty() {
        return Ok(McpControlReply::Status(Vec::new()));
    }
    let readbacks = futures::future::join_all(
        groups
            .into_iter()
            .map(|(args, modes)| one_readback(exe, cwd, kill_grace, args, modes)),
    )
    .await;
    let mut out = Vec::with_capacity(readbacks.len());
    for readback in readbacks {
        out.push(readback?);
    }
    Ok(McpControlReply::Status(out))
}

/// Claude's toggle is a per-project preference; it is meaningful for servers
/// the user configured (user/project/local), not for managed, account,
/// plugin, SDK or explicit launch (`dynamic`) servers.
pub(crate) fn toggleable_scope(scope: Option<&str>) -> bool {
    matches!(scope, Some("user" | "project" | "local"))
}

pub(crate) const TOGGLE_SCOPE_REASON: &str = "Only MCP servers from user, project or local Claude configuration can be enabled or disabled per project here.";

/// Toggles act on Claude's own per-cwd preference, which needs the server to
/// be loaded: use the unrestricted (Full-mode) flags without any grant.
fn unrestricted_args() -> Vec<&'static str> {
    admin_policy_args(PermissionMode::Full)
}

async fn set_enabled(
    exe: &Path,
    cwd: &Path,
    kill_grace: Duration,
    server: &str,
    enabled: bool,
) -> Result<McpControlReply, HarnessError> {
    let mut process = AdminProcess::spawn(exe, cwd, &unrestricted_args(), kill_grace).await?;
    let result = async {
        process.initialize().await?;
        let (before, _) = process
            .status_until(|servers| {
                servers
                    .iter()
                    .find(|row| row.name == server)
                    .is_none_or(|row| row.status != ProviderMcpStatus::Pending)
            })
            .await?;
        let Some(row) = before.iter().find(|row| row.name == server) else {
            return Ok(McpControlReply::SetEnabled {
                outcome: McpWriteOutcome::Rejected,
                readback: Some(before),
                message: Some("Claude no longer reports this MCP server for the project.".into()),
            });
        };
        if !toggleable_scope(row.scope.as_deref()) {
            return Ok(McpControlReply::SetEnabled {
                outcome: McpWriteOutcome::Rejected,
                readback: Some(before),
                message: Some(TOGGLE_SCOPE_REASON.into()),
            });
        }
        let answer = process
            .request(
                json!({"subtype": "mcp_toggle", "serverName": server, "enabled": enabled}),
                REQUEST_TIMEOUT,
            )
            .await;
        let (outcome, message) = match answer {
            Ok(Answer::Success(_)) => (McpWriteOutcome::Applied, None),
            Ok(Answer::Error(message)) => (
                McpWriteOutcome::Rejected,
                Some(redact_provider_text(&message, MAX_ERROR_CHARS)),
            ),
            Err(_) => {
                return Ok(McpControlReply::SetEnabled {
                    outcome: McpWriteOutcome::Indeterminate,
                    readback: None,
                    message: Some(
                        "Claude did not confirm the change; it may have been applied.".into(),
                    ),
                });
            }
        };
        let readback = match outcome {
            McpWriteOutcome::Applied => process
                .status_until(|servers| {
                    servers
                        .iter()
                        .find(|row| row.name == server)
                        .is_none_or(|row| row.status != ProviderMcpStatus::Pending)
                })
                .await
                .ok()
                .map(|(servers, _)| servers),
            _ => None,
        };
        Ok(McpControlReply::SetEnabled {
            outcome,
            readback,
            message,
        })
    }
    .await;
    process.finish().await;
    result
}

fn csp_list(csp: Option<&serde_json::Map<String, Value>>, key: &str) -> Vec<String> {
    csp.and_then(|csp| csp.get(key))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| clean_short(Some(entry), 256))
                .filter(|entry| !entry.chars().any(char::is_whitespace))
                .take(MAX_CSP_ENTRIES)
                .collect()
        })
        .unwrap_or_default()
}

fn resource_view(
    server: &str,
    uri: &str,
    payload: &Value,
) -> Result<ProviderMcpAppResourceView, HarnessError> {
    let contents = payload
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            HarnessError::Protocol("Claude returned a malformed resource read".into())
        })?;
    let first = contents.first().and_then(Value::as_object).ok_or_else(|| {
        HarnessError::Protocol("The MCP server returned no resource content".into())
    })?;
    let (bytes, binary, text) = if let Some(text) = first.get("text").and_then(Value::as_str) {
        (text.as_bytes().to_vec(), false, Some(text))
    } else if let Some(blob) = first.get("blob").and_then(Value::as_str) {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(blob.as_bytes())
            .map_err(|_| {
                HarnessError::Protocol("The MCP resource blob is not valid base64".into())
            })?;
        (decoded, true, None)
    } else {
        return Err(HarnessError::Protocol(
            "The MCP resource has neither text nor blob content".into(),
        ));
    };
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let (text_preview, truncated) = match text {
        Some(text) => {
            let preview: String = text
                .chars()
                .take(MAX_PREVIEW_CHARS)
                .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
                .collect();
            (Some(preview), text.chars().nth(MAX_PREVIEW_CHARS).is_some())
        }
        None => (None, false),
    };
    let ui = first
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("ui"))
        .and_then(Value::as_object);
    let csp = ui.and_then(|ui| ui.get("csp")).and_then(Value::as_object);
    let permissions = ui
        .and_then(|ui| ui.get("permissions"))
        .and_then(Value::as_object)
        .map(|permissions| {
            permissions
                .keys()
                .filter(|key| {
                    key.len() <= 64
                        && key
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                })
                .take(16)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(ProviderMcpAppResourceView {
        identity: String::new(),
        server: server.to_owned(),
        uri: first
            .get("uri")
            .and_then(Value::as_str)
            .filter(|returned| valid_ui_uri(returned))
            .unwrap_or(uri)
            .to_owned(),
        mime_type: clean_short(first.get("mimeType"), 128),
        byte_len: bytes.len(),
        sha256,
        binary,
        text_preview,
        truncated,
        content_count: contents.len(),
        csp_connect_domains: csp_list(csp, "connectDomains"),
        csp_resource_domains: csp_list(csp, "resourceDomains"),
        csp_frame_domains: csp_list(csp, "frameDomains"),
        csp_base_uri_domains: csp_list(csp, "baseUriDomains"),
        permissions,
        prefers_border: ui
            .and_then(|ui| ui.get("prefersBorder"))
            .and_then(Value::as_bool),
        rendered: false,
        render_unavailable_reason: APP_RENDER_UNAVAILABLE.into(),
    })
}

async fn read_app_resource(
    exe: &Path,
    cwd: &Path,
    kill_grace: Duration,
    server: &str,
    uri: &str,
) -> Result<McpControlReply, HarnessError> {
    if !valid_ui_uri(uri) {
        return Err(HarnessError::Protocol(
            "Only MCP Apps ui:// resources can be read here".into(),
        ));
    }
    let mut process = AdminProcess::spawn(exe, cwd, &unrestricted_args(), kill_grace).await?;
    let result = async {
        process.initialize().await?;
        // The server must have finished connecting before a read can succeed,
        // and only a URI one of its own tools declares may be read.
        let (servers, _) = process
            .status_until(|servers| {
                servers
                    .iter()
                    .find(|row| row.name == server)
                    .is_none_or(|row| row.status != ProviderMcpStatus::Pending)
            })
            .await?;
        let row = servers
            .iter()
            .find(|row| row.name == server)
            .ok_or_else(|| {
                HarnessError::Protocol(
                    "Claude does not report this MCP server for the project".into(),
                )
            })?;
        if row.status == ProviderMcpStatus::Connected
            && !row
                .tools
                .iter()
                .any(|tool| tool.ui_resource_uri.as_deref() == Some(uri))
        {
            return Err(HarnessError::Protocol(
                "None of this server's tools declares that MCP App resource".into(),
            ));
        }
        match process
            .request(
                json!({"subtype": "mcp_read_resource", "serverName": server, "uri": uri}),
                READ_TIMEOUT,
            )
            .await
        {
            Ok(Answer::Success(payload)) => resource_view(server, uri, &payload)
                .map(|view| McpControlReply::AppResource(Box::new(view))),
            Ok(Answer::Error(message)) if message.contains(UNSUPPORTED_SUBTYPE) => {
                Err(HarnessError::Protocol(
                    "This Claude CLI does not support MCP Apps resource reads".into(),
                ))
            }
            Ok(Answer::Error(message)) => Err(HarnessError::Protocol(redact_provider_text(
                &message,
                MAX_ERROR_CHARS,
            ))),
            Err(failure) => Err(failure.into_error("mcp_read_resource")),
        }
    }
    .await;
    process.finish().await;
    result
}
