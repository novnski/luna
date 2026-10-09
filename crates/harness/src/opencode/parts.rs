//! Per-part streaming state: text/reasoning/tool snapshots and deltas.

use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};

use super::*;

/// Per-part streaming state (dedup between full-part snapshots and deltas).
#[derive(Default)]
pub(super) struct PartState {
    /// "text" | "reasoning" | "tool".
    pub(super) kind: String,
    /// Bytes of part text already emitted (snapshots resend the full text).
    pub(super) emitted: usize,
    /// Digest of the text already emitted: a snapshot whose prefix differs
    /// is a rewrite, never extended with a mismatched suffix.
    pub(super) digest: Sha256,
    /// An ended text/reasoning snapshot is final. Reconnect readback can
    /// overtake deltas already queued on the live subscription.
    pub(super) text_done: bool,
    pub(super) tool_started: bool,
    pub(super) tool_done: bool,
    /// The native tool name and complete arguments, remembered from the
    /// call for the result projection (terminal frames carry neither).
    pub(super) tool: String,
    pub(super) input: Value,
    /// The typed call of the first chip, and every chip id the call opened
    /// (one per file operation for a multi-file patch).
    pub(super) call: Option<ToolCall>,
    pub(super) chips: Vec<String>,
    /// A running shell call's live output capture, and its summary once
    /// drained.
    pub(super) shell: Option<ShellTail>,
    pub(super) live: Option<LiveOutput>,
}

/// Streaming state for one opencode session's feed (ours or a child's).
#[derive(Default)]
pub(super) struct SessionFeed {
    /// Messages whose step this run saw start (root feed only): a tool
    /// settled under any other message belongs to a turn that already ended.
    pub(super) assistant_messages: HashSet<String>,
    pub(super) parts: HashMap<String, PartState>,
    /// Configured MCP server names (`GET /api/mcp`), for typing
    /// `<server>_<tool>` calls, and how often they were looked up.
    pub(super) mcp_servers: Vec<String>,
    pub(super) mcp_lookups: u8,
}

/// Rewritten text snapshots are diagnosed (lengths only), a bounded number
/// of times per process.
const MAX_REWRITE_DIAGNOSTICS: usize = 16;

fn note_text_rewrite(kind: &str, emitted: usize, snapshot: usize) {
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    if SEEN.fetch_add(1, Ordering::Relaxed) >= MAX_REWRITE_DIAGNOSTICS {
        return;
    }
    tracing::info!(
        target: "luna_harness::opencode",
        kind,
        emitted,
        snapshot,
        "OpenCode rewrote a streamed item; the transcript keeps the streamed text"
    );
}

/// A part snapshot: emit whatever text extends what already streamed, open /
/// resolve tool chips. Snapshots and deltas interleave — `emitted` (bytes of
/// part text already sent) is the dedup line between them.
pub(super) fn part_snapshot_events(
    feed: &mut SessionFeed,
    part: &Value,
    is_main: bool,
) -> Vec<AgentEvent> {
    let Some(part_id) = part.get("id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let message_id = part
        .get("messageID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "text" | "reasoning" => {
            // OpenCode 2 streams text and reasoning only inside assistant
            // messages (`assistantMessageID`): they render on arrival, even
            // when the message's step start was lost in a bus drop.
            let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
            let entry = feed
                .parts
                .entry(part_id.to_owned())
                .or_insert_with(|| PartState {
                    kind: kind.to_owned(),
                    ..PartState::default()
                });
            if entry.text_done {
                return Vec::new();
            }
            entry.text_done = part.get("completed").and_then(Value::as_bool) == Some(true);
            // The transcript is append-only: a snapshot that extends what
            // streamed adds its suffix; a rewrite (shorter, mid-character
            // or a different prefix) keeps the streamed text and is
            // diagnosed, never answered with a mismatched suffix.
            let rewritten = text.get(..entry.emitted).is_none_or(|prefix| {
                entry.emitted > 0
                    && Sha256::digest(prefix.as_bytes()) != entry.digest.clone().finalize()
            });
            if rewritten {
                note_text_rewrite(kind, entry.emitted, text.len());
                return Vec::new();
            }
            let Some(suffix) = text
                .get(entry.emitted..)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            else {
                return Vec::new();
            };
            entry.emitted = text.len();
            entry.digest.update(suffix.as_bytes());
            vec![if entry.kind == "reasoning" {
                AgentEvent::ReasoningDelta { text: suffix }
            } else {
                AgentEvent::TextDelta { text: suffix }
            }]
        }
        "tool" => {
            let tool = part.get("tool").and_then(Value::as_str).unwrap_or_default();
            let status = part
                .get("state")
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input = part
                .get("state")
                .and_then(|s| s.get("input"))
                .cloned()
                .unwrap_or(Value::Null);
            // Chips key on callID (the v2 `session:message:call` id, which
            // a spawn's child binding and settlement key on as well).
            let call_id = part
                .get("callID")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(part_id)
                .to_owned();
            // A call first seen at its end, under an assistant message this
            // run never saw start, belongs to a turn that already ended:
            // OpenCode settles a killed server's dangling call
            // (`session.tool.failed`, `aborted`) when the session next
            // executes. That turn ended with its chip unresolved, as any run
            // that dies mid-tool; the late settlement opens no nameless chip
            // in the current turn (host-journal evidence only).
            if is_main
                && matches!(status, "completed" | "error")
                && !feed.parts.contains_key(part_id)
                && !feed.assistant_messages.contains(message_id)
            {
                let state = part.get("state").unwrap_or(&Value::Null);
                return vec![receipt_evidence(
                    "tool.late_settlement",
                    Some(&call_id),
                    json!({
                        "status": status,
                        "errorType": state.get("errorType"),
                        "executed": state.get("executed"),
                    }),
                )];
            }
            let entry = feed
                .parts
                .entry(part_id.to_owned())
                .or_insert_with(|| PartState {
                    kind: "tool".to_owned(),
                    ..PartState::default()
                });
            let mut events = Vec::new();
            let has_input = input.as_object().is_some_and(|o| !o.is_empty());
            // `called` (running) carries the complete arguments; a call
            // whose arguments are empty still opens its chip there.
            if !entry.tool_started
                && (has_input || matches!(status, "running" | "completed" | "error"))
            {
                entry.tool_started = true;
                entry.tool = tool.to_owned();
                entry.input = input.clone();
                let chips = tool_chips(&call_id, tool, &input, &feed.mcp_servers);
                entry.call = chips.first().map(|chip| chip.call.clone());
                entry.chips = chips.iter().map(|chip| chip.id.clone()).collect();
                // A spawn (`subagent`/`task`) registers with the child tree
                // from the raw v2 observation of the same frame, in any
                // session of the run (`subagents`).
                events.extend(chips.into_iter().map(|chip| AgentEvent::ToolCall {
                    id: chip.id,
                    call: chip.call,
                }));
            }
            if entry.tool_started && !entry.tool_done && matches!(status, "completed" | "error") {
                entry.tool_done = true;
                let state = part.get("state").unwrap_or(&Value::Null);
                events.extend(tool_result_events(&call_id, entry, state));
                // Nothing of the call is needed past its result.
                entry.input = Value::Null;
                entry.shell = None;
            }
            events
        }
        // step-start / step-finish / snapshot / patch bookkeeping: not
        // transcript.
        _ => Vec::new(),
    }
}

/// A text delta appends to its part. Deltas follow the part's opening
/// `message.part.updated` (which fixes the kind); an unknown part defaults
/// to assistant text.
pub(super) fn part_delta_events(
    feed: &mut SessionFeed,
    part_id: &str,
    delta: &str,
) -> Vec<AgentEvent> {
    if delta.is_empty() {
        return Vec::new();
    }
    let entry = feed
        .parts
        .entry(part_id.to_owned())
        .or_insert_with(|| PartState {
            kind: "text".to_owned(),
            ..PartState::default()
        });
    if entry.kind == "tool" || entry.text_done {
        return Vec::new();
    }
    entry.emitted += delta.len();
    entry.digest.update(delta.as_bytes());
    vec![if entry.kind == "reasoning" {
        AgentEvent::ReasoningDelta {
            text: delta.to_owned(),
        }
    } else {
        AgentEvent::TextDelta {
            text: delta.to_owned(),
        }
    }]
}
