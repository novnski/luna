//! Live shell output and full shell output (OC-06).
//!
//! OpenCode 2.0.22's `shell` tool streams nothing over the event bus: it
//! publishes one `session.tool.progress {shellID}` right after spawning,
//! and its terminal result carries only the bounded tail (the last 50 KiB /
//! 2,000 lines plus a "full output saved to …" notice). The complete
//! combined stdout/stderr lives in the shell service's file, which the
//! official clients read with `GET /api/shell/{id}/output?cursor=&limit=`
//! ("clients poll `output` for file-backed output by cursor",
//! `core/src/shell.ts`). LUNA uses that native API:
//! - while the call runs, a poller reads new bytes by cursor and posts them
//!   into the run's own bus channel (`tool.shell.output`), so the run loop
//!   emits them as `ToolActivity{Stdout}` in order, sharing the call's chip
//!   id: the live tail, exactly like Codex's command output deltas;
//! - when the terminal frame arrives the loop stops the poller and drains
//!   the rest itself before emitting the `ToolResult`, so activity never
//!   follows its result or the run's `Done`, and the host journal holds the
//!   complete output (head, body and tail);
//! - the result envelope's `liveOutput` records the byte count, SHA-256 and
//!   whether the capture is complete (a removed shell, a cap or a cursor gap
//!   say why not).
//!
//! The bytes are the combined stream exactly as received (control
//! sequences included); the document keeps its own 2 KiB live tail.

use sha2::{Digest, Sha256};

use super::*;
use crate::CancellationToken;

/// Poll cadence while a shell runs and has no new output.
const POLL_INTERVAL: Duration = Duration::from_millis(400);
/// Bytes per output page (the shell API's own default limit).
const PAGE_BYTES: u64 = 64 * 1024;
/// Per-call ceiling on streamed output (journal growth guard). Past it the
/// capture is marked incomplete; the native file reference stays.
const MAX_LIVE_BYTES: u64 = 16 * 1024 * 1024;
/// Consecutive failed reads after which a poller gives up.
const MAX_POLL_FAILURES: u32 = 10;
/// Bound for one output read.
const SHELL_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Re-reads while the output file trails its reported size at settlement.
const DRAIN_STALL_RETRIES: u32 = 5;
const DRAIN_STALL_DELAY: Duration = Duration::from_millis(50);

/// What the envelope says about a shell call's captured output.
pub(super) struct LiveOutput {
    shell_id: String,
    bytes: u64,
    sha256: String,
    complete: bool,
    reason: Option<&'static str>,
}

impl LiveOutput {
    pub(super) fn has_bytes(&self) -> bool {
        self.bytes > 0
    }

    pub(super) fn envelope(&self) -> Value {
        let mut out = json!({
            "source": "opencode-shell",
            "shellID": self.shell_id,
            "byteLength": self.bytes,
            "sha256": self.sha256,
            "complete": self.complete,
        });
        if let Some(reason) = self.reason {
            out["reason"] = json!(reason);
        }
        out
    }
}

/// Per-call capture state: the byte cursor the run loop has applied, the
/// running digest, and the poller feeding it.
pub(super) struct ShellTail {
    shell_id: String,
    applied: u64,
    streamed: u64,
    hasher: Sha256,
    capped: bool,
    gap: bool,
    poller: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
}

impl Drop for ShellTail {
    fn drop(&mut self) {
        self.stop_poller();
    }
}

impl ShellTail {
    fn new(shell_id: &str) -> Self {
        Self {
            shell_id: shell_id.to_owned(),
            applied: 0,
            streamed: 0,
            hasher: Sha256::new(),
            capped: false,
            gap: false,
            poller: None,
        }
    }

    fn stop_poller(&mut self) {
        if let Some((cancel, handle)) = self.poller.take() {
            cancel.cancel();
            handle.abort();
        }
    }

    /// Apply one page read from `start` to `end`. Bytes the loop already
    /// applied (a page queued by the poller and then re-read by the final
    /// drain) are skipped; a forward gap is accepted but recorded.
    fn apply(&mut self, start: u64, end: u64, text: &str) -> Option<String> {
        if end <= self.applied || self.capped {
            return None;
        }
        let mut text = text;
        if start < self.applied {
            // Overlap: only a valid UTF-8 page maps bytes to characters.
            let skip = (self.applied - start) as usize;
            if text.len() as u64 != end - start || !text.is_char_boundary(skip) {
                return None;
            }
            text = &text[skip..];
        } else if start > self.applied {
            self.gap = true;
        }
        self.applied = end;
        if text.is_empty() {
            return None;
        }
        self.streamed += text.len() as u64;
        self.hasher.update(text.as_bytes());
        if self.streamed >= MAX_LIVE_BYTES {
            self.capped = true;
        }
        Some(text.to_owned())
    }

    fn summary(&self, reason: Option<&'static str>) -> LiveOutput {
        let reason = reason
            .or(self.capped.then_some("live output cap reached"))
            .or(self.gap.then_some("output cursor gap"));
        LiveOutput {
            shell_id: self.shell_id.clone(),
            bytes: self.streamed,
            sha256: format!("{:x}", self.hasher.clone().finalize()),
            complete: reason.is_none(),
            reason,
        }
    }
}

/// OpenCode shell ids (`sh_…`, `^sh_` in the API) used in a request path.
fn valid_shell_id(id: &str) -> bool {
    id.len() > 3
        && id.len() <= 64
        && id.starts_with("sh_")
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The shell id of a call: its progress/result metadata, else the native
/// truncation notice (`full output saved to …/sh_….out`).
fn shell_id_of(state: &Value) -> Option<String> {
    if let Some(id) = state
        .pointer("/metadata/shellID")
        .and_then(Value::as_str)
        .filter(|id| valid_shell_id(id))
    {
        return Some(id.to_owned());
    }
    state
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .find_map(|text| {
            let at = text.rfind("full output saved to ")?;
            let path = text[at..].split(']').next()?;
            let file = path.rsplit('/').next()?;
            let id = file.strip_suffix(".out")?;
            valid_shell_id(id).then(|| id.to_owned())
        })
}

struct ShellPage {
    start: u64,
    end: u64,
    size: u64,
    text: String,
}

enum PageError {
    /// The shell (or its output) is gone: 404.
    Gone,
    Failed,
}

/// One output page from `cursor`. The server decodes each page as UTF-8, so
/// a character split at the page end comes back as U+FFFD; unless
/// `accept_split` is set, such a trailing partial character is left for the
/// next read (only when everything before it decoded cleanly, so byte and
/// character offsets still agree).
async fn read_page(
    server: &Server,
    directory: Option<&str>,
    shell_id: &str,
    cursor: u64,
    accept_split: bool,
) -> Result<ShellPage, PageError> {
    let path = format!("/api/shell/{shell_id}/output?cursor={cursor}&limit={PAGE_BYTES}");
    let (status, body) = server
        .exchange(
            reqwest::Method::GET,
            &path,
            directory,
            None,
            Some(SHELL_READ_TIMEOUT),
        )
        .await
        .map_err(|_| PageError::Failed)?;
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(PageError::Gone);
    }
    if !status.is_success() {
        return Err(PageError::Failed);
    }
    let data = serde_json::from_str::<Value>(&body)
        .map(unwrap_data)
        .map_err(|_| PageError::Failed)?;
    let (Some(mut text), Some(end), Some(size)) = (
        data.get("output")
            .and_then(Value::as_str)
            .map(str::to_owned),
        data.get("cursor").and_then(Value::as_u64),
        data.get("size").and_then(Value::as_u64),
    ) else {
        return Err(PageError::Failed);
    };
    let start = cursor.min(end);
    let mut end = end;
    if !accept_split && text.ends_with('\u{FFFD}') {
        let kept = text.trim_end_matches('\u{FFFD}').len();
        let consumed = end - start;
        let trailing = consumed.saturating_sub(kept as u64);
        if (1..=3).contains(&trailing) && !text[..kept].contains('\u{FFFD}') {
            text.truncate(kept);
            end = start + kept as u64;
        }
    }
    Ok(ShellPage {
        start,
        end,
        size,
        text,
    })
}

/// The poller: new bytes by cursor into the run's bus channel until it is
/// cancelled, the shell is gone, the channel closes or the cap is reached.
fn spawn_poller(
    server: Server,
    directory: Option<String>,
    session: String,
    call_id: String,
    shell_id: String,
    bus: mpsc::Sender<BusMsg>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cursor = 0u64;
        let mut failures = 0u32;
        loop {
            if cancel.is_cancelled() || bus.is_closed() || cursor >= MAX_LIVE_BYTES {
                return;
            }
            let page = tokio::select! {
                _ = cancel.cancelled() => return,
                page = read_page(&server, directory.as_deref(), &shell_id, cursor, false) => page,
            };
            let mut more = false;
            match page {
                Ok(page) => {
                    failures = 0;
                    if page.end > cursor {
                        more = page.end < page.size;
                        let message = json!({
                            "type": "tool.shell.output",
                            "properties": {
                                "sessionID": session,
                                "callID": call_id,
                                "start": page.start,
                                "end": page.end,
                                "text": page.text,
                            }
                        });
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            sent = bus.send(BusMsg::Event(message)) => {
                                if sent.is_err() {
                                    return;
                                }
                            }
                        }
                        cursor = page.end;
                    }
                }
                Err(PageError::Gone) => return,
                Err(PageError::Failed) => {
                    failures += 1;
                    if failures >= MAX_POLL_FAILURES {
                        return;
                    }
                }
            }
            if !more {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                }
            }
        }
    })
}

/// Shared routing context for the tool side-channel payloads.
pub(super) struct ToolSideCtx<'a> {
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) bus_tx: &'a mpsc::Sender<BusMsg>,
}

/// `tool.progress` (live replacement metadata of a running call):
/// a bounded `ToolActivity{Progress}` status line where the metadata says
/// something displayable, and a shell's `shellID` starts its live tail.
/// Spawn progress (`subagent`) is left to the subagent binding.
pub(super) fn tool_progress_events(
    feed: &mut SessionFeed,
    props: &Value,
    ctx: &ToolSideCtx<'_>,
) -> Vec<AgentEvent> {
    let Some(call_id) = props.get("callID").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(entry) = feed.parts.get_mut(call_id) else {
        return Vec::new();
    };
    if entry.tool_done || !entry.tool_started {
        return Vec::new();
    }
    let metadata = props.get("metadata").unwrap_or(&Value::Null);
    if entry.tool == "shell" {
        if entry.shell.is_none()
            && let Some(shell_id) = metadata
                .get("shellID")
                .and_then(Value::as_str)
                .filter(|id| valid_shell_id(id))
        {
            let mut tail = ShellTail::new(shell_id);
            let cancel = CancellationToken::new();
            let handle = spawn_poller(
                ctx.server.detached(),
                ctx.dir.map(str::to_owned),
                props
                    .get("sessionID")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                call_id.to_owned(),
                shell_id.to_owned(),
                ctx.bus_tx.clone(),
                cancel.clone(),
            );
            tail.poller = Some((cancel, handle));
            entry.shell = Some(tail);
        }
        return Vec::new();
    }
    progress_line(&entry.tool, metadata)
        .map(|text| AgentEvent::ToolActivity {
            id: call_id.to_owned(),
            kind: luna_proto::ToolActivityKind::Progress,
            text,
        })
        .into_iter()
        .collect()
}

/// Longest progress status line (display-only; replaced by the result).
const MAX_PROGRESS_CHARS: usize = 200;

fn progress_line(tool: &str, metadata: &Value) -> Option<String> {
    let bounded = |text: String| -> Option<String> {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_PROGRESS_CHARS)
            .collect();
        (!text.trim().is_empty()).then_some(text)
    };
    match tool {
        "subagent" | "task" | "shell" => None,
        "websearch" => metadata
            .get("provider")
            .and_then(Value::as_str)
            .and_then(|provider| bounded(format!("Searching with {provider}"))),
        // Code Mode reports its nested calls as replacement metadata.
        "execute" => {
            let calls = metadata.get("toolCalls")?.as_array()?;
            let names: Vec<String> = calls
                .iter()
                .take(8)
                .map(|call| {
                    let name = call.get("tool").and_then(Value::as_str).unwrap_or("tool");
                    match call.get("status").and_then(Value::as_str) {
                        Some("completed") => format!("{name} ✓"),
                        Some("error") => format!("{name} ✗"),
                        _ => format!("{name} …"),
                    }
                })
                .collect();
            let count = calls.len();
            bounded(format!(
                "{count} tool call{}: {}",
                if count == 1 { "" } else { "s" },
                names.join(", ")
            ))
        }
        _ => ["title", "message", "status"]
            .iter()
            .find_map(|key| metadata.get(*key).and_then(Value::as_str))
            .and_then(|text| bounded(text.to_owned())),
    }
}

/// `tool.shell.output` (a page the poller read): `ToolActivity{Stdout}` on
/// the call's chip while the call still runs.
pub(super) fn shell_output_events(feed: &mut SessionFeed, props: &Value) -> Vec<AgentEvent> {
    let (Some(call_id), Some(start), Some(end), Some(text)) = (
        props.get("callID").and_then(Value::as_str),
        props.get("start").and_then(Value::as_u64),
        props.get("end").and_then(Value::as_u64),
        props.get("text").and_then(Value::as_str),
    ) else {
        return Vec::new();
    };
    let Some(entry) = feed.parts.get_mut(call_id) else {
        return Vec::new();
    };
    if entry.tool_done {
        return Vec::new();
    }
    let Some(tail) = entry.shell.as_mut() else {
        return Vec::new();
    };
    tail.apply(start, end, text)
        .map(|text| AgentEvent::ToolActivity {
            id: call_id.to_owned(),
            kind: luna_proto::ToolActivityKind::Stdout,
            text,
        })
        .into_iter()
        .collect()
}

/// Work a tool part needs before `part_snapshot_events` types or settles
/// it: the MCP server names its chip may depend on, and, for a terminal
/// `shell` frame, the final drain of its output (returned as
/// `ToolActivity{Stdout}` events that must precede the result).
pub(super) async fn prepare_tool_part(
    feed: &mut SessionFeed,
    part: &Value,
    ctx: &ToolSideCtx<'_>,
) -> Vec<AgentEvent> {
    if part.get("type").and_then(Value::as_str) != Some("tool") {
        return Vec::new();
    }
    let Some(part_id) = part.get("id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let state = part.get("state").unwrap_or(&Value::Null);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let started = feed.parts.get(part_id).is_some_and(|p| p.tool_started);
    let tool = feed
        .parts
        .get(part_id)
        .filter(|p| p.tool_started)
        .map(|p| p.tool.clone())
        .unwrap_or_else(|| {
            part.get("tool")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        });

    if !started
        && matches!(status, "running" | "completed" | "error")
        && needs_mcp_servers(&tool)
        && (feed.mcp_lookups == 0
            || (mcp_split(&tool, &feed.mcp_servers).is_none()
                && feed.mcp_lookups < MAX_MCP_LOOKUPS))
    {
        feed.mcp_lookups += 1;
        feed.mcp_servers = ctx.server.mcp_server_names(ctx.dir).await;
    }

    if tool != "shell" || !matches!(status, "completed" | "error") {
        return Vec::new();
    }
    let Some(entry) = feed.parts.get_mut(part_id) else {
        return Vec::new();
    };
    if entry.tool_done || entry.live.is_some() {
        return Vec::new();
    }
    let is_error = status == "error";
    let truncated = state
        .pointer("/metadata/truncated")
        .and_then(Value::as_bool)
        == Some(true);
    let background = state.pointer("/metadata/status").and_then(Value::as_str) == Some("running");
    if entry.shell.is_none()
        && (truncated || is_error)
        && !background
        && let Some(shell_id) = shell_id_of(state)
    {
        entry.shell = Some(ShellTail::new(&shell_id));
    }
    let Some(mut tail) = entry.shell.take() else {
        return Vec::new();
    };
    tail.stop_poller();
    if background {
        // Still running after the call returned: a background task's
        // output, not this call's result.
        entry.live = Some(tail.summary(Some("command moved to the background")));
        return Vec::new();
    }
    let mut events = Vec::new();
    let mut reason = None;
    let mut stalls = 0u32;
    loop {
        if tail.capped {
            break;
        }
        let before = tail.applied;
        match read_page(ctx.server, ctx.dir, &tail.shell_id, before, stalls > 0).await {
            Ok(page) => {
                if let Some(text) = tail.apply(page.start, page.end, &page.text) {
                    events.push(AgentEvent::ToolActivity {
                        id: part_id.to_owned(),
                        kind: luna_proto::ToolActivityKind::Stdout,
                        text,
                    });
                }
                if tail.applied >= page.size {
                    break;
                }
                if tail.applied <= before {
                    stalls += 1;
                    if stalls > DRAIN_STALL_RETRIES {
                        reason = Some("shell output stopped advancing");
                        break;
                    }
                    tokio::time::sleep(DRAIN_STALL_DELAY).await;
                } else {
                    stalls = 0;
                }
            }
            Err(PageError::Gone) => {
                reason = Some("shell output no longer available");
                break;
            }
            Err(PageError::Failed) => {
                reason = Some("shell output read failed");
                break;
            }
        }
    }
    entry.live = Some(tail.summary(reason));
    events
}

/// MCP server-list lookups per feed: the first, plus refreshes for names
/// that match no known server (a server added mid-run).
const MAX_MCP_LOOKUPS: u8 = 4;
