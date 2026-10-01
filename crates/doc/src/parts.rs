//! Message parts: the event fold, the render-only privacy policy, and continuation splitting.
//!
//! Ports of `packages/control/src/parts.ts` (fold) and
//! `packages/session-doc/src/{render-parts,messages}.ts`.

use serde::{Deserialize, Serialize};

use luna_proto::{
    AgentEvent, SUBAGENT_INPUT_KEEP, ToolActivityKind, ToolCall, ToolDiff, UserInputQuestion,
};

use crate::constants::MSG_INLINE_MAX;

/// Char cap for the tool-output SUMMARY persisted into the doc: first
/// non-empty line, nothing more. The per-part 4KB cap (c951c3e) bounded each
/// part but not the session — chat 1b65e93d measured 917KB (85%) of a 1MB doc
/// in capped outputs across 426 tool parts (docs/chat2-sync.md). Full outputs
/// live in the R2 sidecar behind `output_ref`; t3code ships an 84-char
/// summary, so 160 is generous.
pub const TOOL_OUTPUT_SUMMARY_MAX: usize = 160;

/// The doc-resident form of a tool output (docs/chat2-sync.md A1). Full text
/// stays in the host's local run journal and can be fetched when expanded.
///
/// - Markdown code fences are stripped first — ACP harnesses fence every
///   output, so the fence is transport wrapping, never content (pre-fix,
///   every summary read "```console…").
/// - Outputs that fit [`TOOL_OUTPUT_SUMMARY_MAX`] chars ride whole — a
///   summary of a two-line output is more UI than the output.
/// - Bigger outputs keep the first non-empty line, capped, with a `…`
///   marker meaning "there was more".
///
/// `None` for blank output.
pub fn summarize_tool_output(text: &str) -> Option<String> {
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect();
    let stripped = kept.join("\n");
    let stripped = stripped.trim();
    if stripped.is_empty() {
        return None;
    }
    if stripped.chars().count() <= TOOL_OUTPUT_SUMMARY_MAX {
        return Some(stripped.to_owned());
    }
    // Too big to inline: first non-empty line, capped. There is always more
    // than the summary here (whole output exceeded the budget), so the
    // marker is unconditional.
    let line = stripped
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(stripped)
        .trim_end();
    let mut chars = 0usize;
    let mut end = line.len();
    for (i, _) in line.char_indices() {
        if chars == TOOL_OUTPUT_SUMMARY_MAX {
            end = i;
            break;
        }
        chars += 1;
    }
    let mut out = line[..end].to_owned();
    out.push('…');
    Some(out)
}

/// Bounded live display tail. Complete stdout/stdin is retained in the host
/// journal, including control sequences that the transcript never executes.
pub const TOOL_ACTIVITY_PREVIEW_BYTES: usize = 2048;

fn activity_tail(previous: Option<&str>, text: &str, kind: ToolActivityKind) -> Option<String> {
    let mut joined = previous.unwrap_or_default().to_owned();
    if kind == ToolActivityKind::Stdin {
        joined.push_str("\nstdin › ");
    }
    // Strip terminal escape/control sequences from the display-only copy.
    let mut escape = 0;
    for ch in text.chars() {
        match escape {
            1 if ch == '[' => {
                escape = 2;
                continue;
            }
            1 if ch == ']' => {
                escape = 3;
                continue;
            }
            1 => {
                escape = 0;
                continue;
            }
            2 => {
                if ('@'..='~').contains(&ch) {
                    escape = 0;
                }
                continue;
            }
            3 => {
                if ch == '\x07' {
                    escape = 0;
                } else if ch == '\x1b' {
                    escape = 4;
                }
                continue;
            }
            4 => {
                escape = if ch == '\\' { 0 } else { 3 };
                continue;
            }
            _ => {}
        }
        if ch == '\x1b' {
            escape = 1;
            continue;
        }
        if !ch.is_control() || ch == '\n' || ch == '\t' {
            joined.push(ch);
        }
        // Trim while iterating so even a megabyte delta has a bounded preview.
        if joined.len() > TOOL_ACTIVITY_PREVIEW_BYTES + 256 {
            let mut start = joined.len() - TOOL_ACTIVITY_PREVIEW_BYTES;
            while !joined.is_char_boundary(start) {
                start += 1;
            }
            joined.drain(..start);
        }
    }
    if joined.len() > TOOL_ACTIVITY_PREVIEW_BYTES {
        let mut start = joined.len() - TOOL_ACTIVITY_PREVIEW_BYTES;
        while !joined.is_char_boundary(start) {
            start += 1;
        }
        joined.drain(..start);
    }
    (!joined.trim().is_empty()).then_some(joined)
}

/// Only type/count metadata enters the synced transcript for rich/private
/// results. Arguments, result text, URIs, errors and encoded media stay local.
pub fn rich_tool_output_summary(text: &str, is_error: bool) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let payload = value.get("lunaToolOutput")?;
    let kind = payload.get("kind")?.as_str()?;
    if kind == "unsupported" {
        return Some("Unsupported item · diagnostic available".into());
    }
    if !matches!(kind, "mcp" | "dynamic") {
        return None;
    }
    let blocks = if kind == "mcp" {
        payload.pointer("/result/content")
    } else {
        payload.get("contentItems")
    };
    let mut labels = Vec::new();
    for (label, types) in [
        ("text", &["text", "inputText"][..]),
        ("image", &["image", "inputImage"][..]),
        ("audio", &["audio", "inputAudio"][..]),
        ("resource", &["resource", "resource_link"][..]),
    ] {
        let count = blocks
            .and_then(serde_json::Value::as_array)
            .map_or(0, |blocks| {
                blocks
                    .iter()
                    .filter(|block| {
                        block
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|t| types.contains(&t))
                    })
                    .count()
            });
        if count > 0 {
            labels.push(format!("{count} {label}"));
        }
    }
    if payload
        .pointer("/result/structuredContent")
        .or_else(|| payload.pointer("/result/structured_content"))
        .is_some_and(|v| !v.is_null())
    {
        labels.push("structured content".into());
    }
    if payload.get("mcpAppUi").is_some_and(|v| !v.is_null()) {
        labels.push("app resource".into());
    }
    let status = if is_error { "Failed" } else { "Completed" };
    Some(if labels.is_empty() {
        format!("{status} · result available")
    } else {
        format!("{status} · {}", labels.join(" · "))
    })
}

/// Per-file diff stats persisted in place of inline diff text (t3's shape).
/// The inline diff was the bigger bomb than outputs — 32KB/edit, unexercised
/// only because the claude harness emits none. Full diff text lives in the
/// sidecar behind `diff_ref`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiffStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// Line-level add/delete counts for one file's diff.
pub fn diff_stat(diff: &ToolDiff) -> ToolDiffStat {
    let (additions, deletions) = if let Some(patch) = &diff.unified_diff {
        let mut in_hunk = false;
        let (mut additions, mut deletions) = (0, 0);
        for line in patch.lines() {
            if line.starts_with("@@") {
                in_hunk = true;
            } else if line.starts_with("diff --git ") {
                in_hunk = false;
            } else if in_hunk && line.starts_with('+') {
                additions += 1;
            } else if in_hunk && line.starts_with('-') {
                deletions += 1;
            }
        }
        (additions, deletions)
    } else {
        match &diff.old_text {
            None => (diff.new_text.lines().count() as u64, 0),
            Some(old) => {
                let text_diff = similar::TextDiff::from_lines(old.as_str(), diff.new_text.as_str());
                let mut additions = 0u64;
                let mut deletions = 0u64;
                for change in text_diff.iter_all_changes() {
                    match change.tag() {
                        similar::ChangeTag::Insert => additions += 1,
                        similar::ChangeTag::Delete => deletions += 1,
                        similar::ChangeTag::Equal => {}
                    }
                }
                (additions, deletions)
            }
        }
    };
    ToolDiffStat {
        path: diff.path.clone(),
        additions,
        deletions,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageStatus {
    Streaming,
    Complete,
    Aborted,
}

/// Lifecycle of a spawned subagent, carried on its spawn chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentStatus {
    Running,
    Idle,
    Waiting,
    Done,
    Failed,
    Interrupted,
    /// Parent event stream ended without a child completion observation.
    Unobserved,
}

/// Provider-observed child details. Usage is the latest reported snapshot,
/// not an aggregate across turns or a cost estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SubagentActivity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Model the provider reported for the child's own turns. A spawn's
    /// requested model stays on its call; this is what actually answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider-reported task total. Never an input/output split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<String>,
    /// The provider's one-line progress summary while the child runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
}

impl SubagentActivity {
    fn observed_text(text: &str) -> Option<String> {
        let mut chars = text.trim().chars();
        let mut bounded: String = chars.by_ref().take(500).collect();
        if chars.next().is_some() {
            bounded.push('…');
        }
        (!bounded.is_empty()).then_some(bounded)
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            // A new assignment clears the previous outcome; the same child
            // keeps answering with the model it last reported.
            AgentEvent::UserMessage { .. } | AgentEvent::Steered { .. } => {
                *self = Self {
                    model: self.model.take(),
                    ..Self::default()
                }
            }
            AgentEvent::ChildProgress {
                model,
                total_tokens,
                tool_uses,
                duration_ms,
                last_tool,
                summary,
                background,
            } => {
                let text = |value: &Option<String>| value.as_deref().and_then(Self::observed_text);
                self.model = text(model).or(self.model.take());
                self.total_tokens = total_tokens.or(self.total_tokens);
                self.tool_uses = tool_uses.or(self.tool_uses);
                self.duration_ms = duration_ms.or(self.duration_ms);
                self.last_tool = text(last_tool).or(self.last_tool.take());
                self.summary = text(summary).or(self.summary.take());
                self.background = background.or(self.background);
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                self.input_tokens = Some(*input_tokens);
                self.output_tokens = Some(*output_tokens);
            }
            AgentEvent::Done { result, error, .. } => {
                self.result = result.as_deref().and_then(Self::observed_text);
                self.error = error.as_deref().and_then(Self::observed_text);
            }
            _ => {}
        }
    }

    pub fn has_observation(&self) -> bool {
        self.result.is_some()
            || self.error.is_some()
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.model.is_some()
            || self.total_tokens.is_some()
            || self.tool_uses.is_some()
            || self.duration_ms.is_some()
            || self.last_tool.is_some()
            || self.summary.is_some()
            || self.background.is_some()
    }
}

/// One rendered part of an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MessagePart {
    Text {
        id: String,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        id: String,
        path: String,
        name: String,
        mime_type: String,
    },
    /// Model thinking. Carries its body in a dedicated doc field (`reasoning`,
    /// never `text`) so pre-reasoning desktop builds — whose unknown-kind
    /// fallback renders `text` as prose — degrade to an invisible empty text
    /// part instead of leaking raw thinking into the transcript. iOS drops
    /// unknown kinds entirely.
    Reasoning {
        id: String,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Tool {
        id: String,
        call: ToolCall,
        #[serde(default)]
        is_error: bool,
        /// True once a ToolResult arrived.
        #[serde(default)]
        resolved: bool,
        /// One-line tool output summary ([`summarize_tool_output`]). Old
        /// entries (pre-strip) still carry up to 4KB here; old app versions
        /// render this field either way, so the strip is invisible to them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Inline file diff — written by pre-strip app versions only; new
        /// folds persist [`Self::Tool::diff_stats`] + `diff_ref` instead.
        /// Kept so old docs render their diffs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<ToolDiff>,
        /// Key (`{chatId}/{partId}`) for lazy full output from the host
        /// journal or a legacy sidecar; the fold is chat-agnostic.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_ref: Option<String>,
        /// Captured-output byte length, so the UI can say "Show full output (12 KB)".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_bytes: Option<u64>,
        /// Sidecar key (`{chatId}/{partId}.diff`) of the full diff JSON.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_ref: Option<String>,
        /// Per-file diff stats (additive replacement for inline `diff`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_stats: Option<Vec<ToolDiffStat>>,
        /// The SUBAGENT doc id this spawn chip indexes (additive; stamped by
        /// the engine like the sidecar refs — the fold is chat-agnostic).
        /// The chip IS the index: the client learns the doc/blob id from it,
        /// there is no listing endpoint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_ref: Option<String>,
        /// Subagent lifecycle, DISTINCT from `resolved`: under the eager-done
        /// policy the spawn tool's own result lands while the subagent still
        /// runs. `running` → the ref is a live doc (watch it); `done`/
        /// `failed` → frozen (blob first, doc as fallback).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_status: Option<SubagentStatus>,
        /// One-line live tail of the subagent's latest output, folded from
        /// its tagged text deltas (capped; display-only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_tail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_activity: Option<SubagentActivity>,
    },
    #[serde(rename_all = "camelCase")]
    Input {
        id: String,
        request_id: String,
        questions: Vec<UserInputQuestion>,
        #[serde(default)]
        resolved: bool,
    },
    /// Codex `request_user_input_async` completes immediately. This durable
    /// card never marks the run AwaitingInput and has no provider resolver.
    #[serde(rename_all = "camelCase")]
    AsyncQuestion {
        id: String,
        questions: Vec<luna_proto::AsyncUserInputQuestion>,
        #[serde(default)]
        resolved: bool,
    },
    Error {
        id: String,
        message: String,
    },
}

impl MessagePart {
    pub fn id(&self) -> &str {
        match self {
            MessagePart::Text { id, .. }
            | MessagePart::Image { id, .. }
            | MessagePart::Reasoning { id, .. }
            | MessagePart::Tool { id, .. }
            | MessagePart::Input { id, .. }
            | MessagePart::AsyncQuestion { id, .. }
            | MessagePart::Error { id, .. } => id,
        }
    }

    pub fn byte_len(&self) -> usize {
        match self {
            MessagePart::Text { text, .. } | MessagePart::Reasoning { text, .. } => text.len(),
            MessagePart::Tool {
                call,
                output,
                diff,
                diff_stats,
                ..
            } => {
                serde_json::to_vec(call).map_or(0, |v| v.len())
                    + output.as_ref().map_or(0, String::len)
                    + diff
                        .as_ref()
                        .map_or(0, |d| serde_json::to_vec(d).map_or(0, |v| v.len()))
                    + diff_stats
                        .as_ref()
                        .map_or(0, |s| serde_json::to_vec(s).map_or(0, |v| v.len()))
            }
            MessagePart::Input { questions, .. } => {
                serde_json::to_vec(questions).map_or(0, |v| v.len())
            }
            MessagePart::AsyncQuestion { questions, .. } => {
                serde_json::to_vec(questions).map_or(0, |v| v.len())
            }
            MessagePart::Image {
                id,
                path,
                name,
                mime_type,
            } => id.len() + path.len() + name.len() + mime_type.len(),
            MessagePart::Error { message, .. } => message.len(),
        }
    }
}

/// Fold one agent event into a parts accumulator, in place.
///
/// In place because the fold runs once per streamed event: rebuilding the
/// accumulator each time made long turns O(n²) in allocations.
///
/// Semantics from luna `foldEventIntoParts`:
/// - `SessionStarted` / `Steered` reset the accumulator (turn boundary — makes replay safe).
/// - `TextDelta` appends to the trailing text part, or starts a new one if the trail is not text
///   (a tool call in between breaks the text block).
/// - `ToolCall` appends, or refreshes in place when the id already exists (SDK retry idempotence).
/// - `ToolResult` marks the matching tool part resolved / errored in place.
/// - `InputRequested` appends an input part; `InputResolved` marks it resolved.
/// - `Error` and `Done{error}` become visible error parts.
pub fn fold_event_into_parts(out: &mut Vec<MessagePart>, event: &AgentEvent) {
    match event {
        AgentEvent::SessionStarted { .. } | AgentEvent::Steered { .. } => {
            out.clear();
        }
        AgentEvent::TextDelta { text } => {
            if let Some(MessagePart::Text { text: tail, .. }) = out.last_mut() {
                tail.push_str(text);
            } else {
                let id = format!("t{}", out.len());
                out.push(MessagePart::Text {
                    id,
                    text: text.clone(),
                });
            }
        }
        AgentEvent::NativeReview {
            id,
            completed,
            text,
        } => {
            let tool_id = format!("{id}:lifecycle");
            if !completed || !out.iter().any(|part| part.id() == tool_id) {
                fold_event_into_parts(
                    out,
                    &AgentEvent::ToolCall {
                        id: tool_id.clone(),
                        call: ToolCall::Unknown {
                            name: "Native code review".into(),
                            input: (!completed).then(|| serde_json::json!({ "target": text })),
                        },
                    },
                );
            }
            if *completed {
                fold_event_into_parts(
                    out,
                    &AgentEvent::ToolResult {
                        id: tool_id,
                        is_error: false,
                        output: None,
                        diff: None,
                    },
                );
                let part_id = format!("{id}:findings");
                let findings = if text.trim().is_empty() {
                    "Review completed without findings text.".to_string()
                } else {
                    format!("### Review findings\n\n{text}")
                };
                if let Some(existing) = out.iter_mut().find(|part| part.id() == part_id) {
                    *existing = MessagePart::Text {
                        id: part_id,
                        text: findings,
                    };
                } else {
                    out.push(MessagePart::Text {
                        id: part_id,
                        text: findings,
                    });
                }
            }
        }
        AgentEvent::GeneratedImage {
            id,
            path,
            name,
            mime_type,
        } => {
            let image = MessagePart::Image {
                id: id.clone(),
                path: path.clone(),
                name: name.clone(),
                mime_type: mime_type.clone(),
            };
            if let Some(existing) = out.iter_mut().find(|p| p.id() == id) {
                *existing = image;
            } else {
                out.push(image);
            }
        }
        AgentEvent::ReasoningDelta { text } => {
            // Empty deltas are liveness heartbeats (redacted thinking) — the
            // engine drops them before the fold, but stay tolerant here too.
            if text.is_empty() {
                return;
            }
            if let Some(MessagePart::Reasoning { text: tail, .. }) = out.last_mut() {
                tail.push_str(text);
            } else {
                let id = format!("r{}", out.len());
                out.push(MessagePart::Reasoning {
                    id,
                    text: text.clone(),
                });
            }
        }
        AgentEvent::ToolCall { id, call } => {
            if let Some(existing) = out.iter_mut().find_map(|p| match p {
                MessagePart::Tool {
                    id: pid, call: c, ..
                } if pid == id => Some(c),
                _ => None,
            }) {
                *existing = call.clone();
            } else {
                out.push(MessagePart::Tool {
                    id: id.clone(),
                    call: call.clone(),
                    is_error: false,
                    resolved: false,
                    output: None,
                    diff: None,
                    output_ref: None,
                    output_bytes: None,
                    diff_ref: None,
                    diff_stats: None,
                    subagent_ref: None,
                    subagent_status: None,
                    subagent_tail: None,
                    subagent_activity: None,
                });
            }
        }
        AgentEvent::ToolActivity { id, kind, text } => {
            for part in out.iter_mut() {
                if let MessagePart::Tool {
                    id: pid,
                    call,
                    resolved,
                    output,
                    output_bytes,
                    diff_stats,
                    ..
                } = part
                    && pid == id
                    && !*resolved
                {
                    if *kind == ToolActivityKind::Patch {
                        if let Ok(diff) = serde_json::from_str::<ToolDiff>(text) {
                            *diff_stats = Some(vec![diff_stat(&diff)]);
                            *output = Some("Patch updated".into());
                        }
                    } else {
                        *output_bytes =
                            Some(output_bytes.unwrap_or(0).saturating_add(text.len() as u64));
                        if !matches!(
                            call,
                            ToolCall::Mcp { .. }
                                | ToolCall::ComputerUse { .. }
                                | ToolCall::Unknown { .. }
                        ) {
                            *output = activity_tail(output.as_deref(), text, *kind);
                        }
                    }
                }
            }
        }
        AgentEvent::ToolResult {
            id,
            is_error,
            output,
            diff,
        } => {
            for p in out.iter_mut() {
                if let MessagePart::Tool {
                    id: pid,
                    call,
                    is_error: e,
                    resolved,
                    output: out_slot,
                    diff: diff_slot,
                    output_bytes,
                    diff_stats,
                    ..
                } = p
                    && pid == id
                {
                    let already_resolved = *resolved;
                    *e = *is_error;
                    *resolved = true;
                    // Preserve a small synced preview for each tool kind;
                    // full text stays in the owning host's journal.
                    let private_result = matches!(
                        call,
                        ToolCall::Mcp { .. }
                            | ToolCall::ComputerUse { .. }
                            | ToolCall::Unknown { .. }
                    );
                    if let Some(full) = output {
                        *out_slot = if private_result {
                            rich_tool_output_summary(full, *is_error)
                        } else {
                            summarize_tool_output(full)
                        };
                        *output_bytes = if output_bytes.is_some()
                            || private_result
                            || out_slot.as_deref() != Some(full.trim())
                        {
                            Some(if already_resolved {
                                output_bytes.unwrap_or(0).max(full.len() as u64)
                            } else {
                                output_bytes.unwrap_or(0).saturating_add(full.len() as u64)
                            })
                        } else {
                            None
                        };
                    }
                    *diff_slot = None;
                    if let Some(diff) = diff {
                        *diff_stats = Some(vec![diff_stat(diff)]);
                    }
                }
            }
        }
        AgentEvent::InputRequested {
            request_id,
            questions,
        } => {
            let id = format!("in-{request_id}");
            if !out.iter().any(|p| p.id() == id) {
                out.push(MessagePart::Input {
                    id,
                    request_id: request_id.clone(),
                    questions: questions.clone(),
                    resolved: false,
                });
            }
        }
        AgentEvent::InputResolved { request_id } => {
            for p in out.iter_mut() {
                if let MessagePart::Input {
                    request_id: rid,
                    resolved,
                    ..
                } = p
                    && rid == request_id
                {
                    *resolved = true;
                }
            }
        }
        AgentEvent::AsyncQuestions {
            message_id,
            questions,
        } => {
            let id = format!("aq-{message_id}");
            if !out.iter().any(|part| part.id() == id) {
                out.push(MessagePart::AsyncQuestion {
                    id,
                    questions: questions.clone(),
                    resolved: false,
                });
            }
        }
        AgentEvent::Error { message } => {
            let id = format!("e{}", out.len());
            out.push(MessagePart::Error {
                id,
                message: message.clone(),
            });
        }
        AgentEvent::Done { error, status, .. } => {
            // An interrupted/failed native review must not retain a running
            // lifecycle chip or suggest that findings were delivered.
            if let Some(MessagePart::Tool {
                resolved, is_error, ..
            }) = out
                .iter_mut()
                .find(|part| part.id() == "native-review:lifecycle")
                && !*resolved
            {
                *resolved = true;
                *is_error = true;
                if *status == luna_proto::DoneStatus::Interrupted {
                    out.push(MessagePart::Text {
                        id: "native-review:outcome".into(),
                        text: "Native review interrupted.".into(),
                    });
                }
            }
            if let Some(message) = error {
                let id = format!("e{}", out.len());
                out.push(MessagePart::Error {
                    id,
                    message: message.clone(),
                });
            }
        }
        // Subagent-attributed CONTENT belongs to the subagent's own doc (the
        // engine routes it there); the parent doc keeps only the spawn chip —
        // which these events refresh in place: LIFECYCLE ONLY. A live tail
        // was tried and rejected: rewriting the chip per delta batch grew
        // the parent doc's oplog for the whole subagent run and rendered as
        // distracting mid-stream fragments (user call, 2026-08-18). The
        // `subagent_tail` field stays in the schema for docs that carry it.
        AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } => {
            let status = match event.as_ref() {
                AgentEvent::Done { status, .. } => Some(match status {
                    luna_proto::DoneStatus::Errored => SubagentStatus::Failed,
                    luna_proto::DoneStatus::Interrupted => SubagentStatus::Interrupted,
                    luna_proto::DoneStatus::Completed => SubagentStatus::Done,
                }),
                // A new assignment reopens a settled chip. Providers may
                // announce it with user text or a confirmed turn boundary.
                AgentEvent::UserMessage { .. }
                | AgentEvent::Steered { .. }
                | AgentEvent::InputResolved { .. } => Some(SubagentStatus::Running),
                AgentEvent::InputRequested { .. } => Some(SubagentStatus::Waiting),
                AgentEvent::ChildStatus { status } => Some(match status {
                    luna_proto::ChildRuntimeStatus::Active => SubagentStatus::Running,
                    luna_proto::ChildRuntimeStatus::Idle => SubagentStatus::Idle,
                }),
                _ => None,
            };
            for p in out.iter_mut() {
                if let MessagePart::Tool {
                    id,
                    call,
                    subagent_status,
                    subagent_activity,
                    ..
                } = p
                    && id == parent_tool_use_id
                    // Genus gate: only a SPAWN call ever carries subagent
                    // lifecycle. A driver keying bug (claude's background
                    // shells settled through the subagent subtype,
                    // 2026-08-20) must not decorate an ordinary chip.
                    && call.is_subagent_spawn()
                {
                    match status {
                        Some(_)
                            if matches!(event.as_ref(), AgentEvent::ChildStatus { .. })
                                && matches!(
                                    subagent_status,
                                    Some(SubagentStatus::Done)
                                        | Some(SubagentStatus::Failed)
                                        | Some(SubagentStatus::Interrupted)
                                ) => {}
                        Some(s) => *subagent_status = Some(s),
                        // Passive events do not resolve a waiting request or
                        // regress a terminal state.
                        None if !matches!(
                            subagent_status,
                            Some(SubagentStatus::Done)
                                | Some(SubagentStatus::Failed)
                                | Some(SubagentStatus::Interrupted)
                                | Some(SubagentStatus::Unobserved)
                                | Some(SubagentStatus::Waiting)
                                | Some(SubagentStatus::Idle)
                        ) =>
                        {
                            *subagent_status = Some(SubagentStatus::Running);
                        }
                        None => {}
                    }
                    if let Some(activity) = subagent_activity.as_mut() {
                        activity.apply(event);
                        if !activity.has_observation() {
                            *subagent_activity = None;
                        }
                    } else {
                        let mut activity = SubagentActivity::default();
                        activity.apply(event);
                        if activity.has_observation() {
                            *subagent_activity = Some(activity);
                        }
                    }
                }
            }
        }
        // AvailableCommands feeds the engine's per-harness command cache, not
        // the transcript. UserMessage becomes its own doc ENTRY (the engine's
        // subagent sink writes it), never a part of the assistant message.
        AgentEvent::AssistantMessageCompleted { .. }
        | AgentEvent::Usage { .. }
        | AgentEvent::ContextUsage { .. }
        | AgentEvent::ContextCompaction { .. }
        | AgentEvent::ProviderCheckpoint { .. }
        | AgentEvent::AvailableCommands { .. }
        | AgentEvent::ProviderTaskChanged { .. }
        | AgentEvent::ProviderTurnStarted { .. }
        | AgentEvent::GoalChanged { .. }
        | AgentEvent::ChildStatus { .. }
        | AgentEvent::ChildProgress { .. }
        | AgentEvent::UserMessage { .. } => {}
    }
}

/// Stamp sidecar keys onto resolved tool parts that have sidecar content.
///
/// Separate from the fold because the fold is chat-agnostic and pure; the
/// caller (who knows the chat id) runs this right after each fold step, before
/// the parts hit the doc. Idempotent. Key shape `{chatId}/{partId}` (+
/// `.diff`) matches the edge's `/blob/{chatId}/{partId}` route.
pub fn apply_sidecar_refs(chat_id: &str, parts: &mut [MessagePart]) {
    for part in parts.iter_mut() {
        if let MessagePart::Tool {
            id,
            output_ref,
            output_bytes,
            diff_ref,
            diff_stats,
            ..
        } = part
        {
            if output_ref.is_none() && output_bytes.is_some() {
                *output_ref = Some(format!("{chat_id}/{id}"));
            }
            if diff_ref.is_none() && diff_stats.is_some() {
                *diff_ref = Some(format!("{chat_id}/{id}.diff"));
            }
        }
    }
}

/// Point full output and diff details at the owning host's run journal.
pub fn apply_journal_output_refs(chat_id: &str, parts: &mut [MessagePart]) {
    for part in parts.iter_mut() {
        if let MessagePart::Tool {
            id,
            output_ref,
            output_bytes,
            diff_ref,
            diff_stats,
            ..
        } = part
        {
            if output_ref.is_none() && output_bytes.is_some() {
                *output_ref = Some(format!("{chat_id}/{id}"));
            }
            if diff_ref.is_none() && diff_stats.is_some() {
                *diff_ref = Some(format!("{chat_id}/{id}.diff"));
            }
        }
    }
}

/// What a [`AgentEvent::ToolResult`] owes the sidecar: the full output text
/// and/or the full diff (as JSON), keyed by part id. `None` when the event
/// carries nothing worth uploading.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarPayload {
    pub part_id: String,
    pub output: Option<String>,
    pub diff: Option<ToolDiff>,
}

pub fn sidecar_payload(event: &AgentEvent) -> Option<SidecarPayload> {
    let AgentEvent::ToolResult {
        id, output, diff, ..
    } = event
    else {
        return None;
    };
    let output = output.clone().filter(|o| !o.trim().is_empty());
    if output.is_none() && diff.is_none() {
        return None;
    }
    Some(SidecarPayload {
        part_id: id.clone(),
        output,
        diff: diff.clone(),
    })
}

/// Render-only privacy policy — strip heavy/sensitive tool inputs before a call enters the doc.
///
/// Keeps: command / path / pattern / url / query / todo items / server+tool names,
/// and a subagent spawn's model/type (see [`spawn_badge`] — a couple of short
/// identifiers the chip names the child by).
/// Drops: WriteFile content, EditFile old/new strings, WebFetch prompt, Mcp/Unknown input.
/// Full inputs remain only in the host's local run journal. Idempotent.
pub fn sanitize_tool_call(call: &ToolCall) -> ToolCall {
    match call {
        ToolCall::WriteFile { path, .. } => ToolCall::WriteFile {
            path: path.clone(),
            content: None,
        },
        ToolCall::EditFile { path, .. } => ToolCall::EditFile {
            path: path.clone(),
            old_string: None,
            new_string: None,
        },
        ToolCall::WebFetch { url, .. } => ToolCall::WebFetch {
            url: url.clone(),
            prompt: None,
        },
        ToolCall::Mcp { server, tool, .. } => ToolCall::Mcp {
            server: server.clone(),
            tool: tool.clone(),
            input: spawn_badge(call),
        },
        ToolCall::ComputerUse { .. } => call.clone(),
        ToolCall::Unknown { name, .. } => ToolCall::Unknown {
            name: name.clone(),
            input: spawn_badge(call),
        },
        other => other.clone(),
    }
}

/// The only slice of a tool input allowed into the doc: a subagent spawn's
/// [`SUBAGENT_INPUT_KEEP`] keys, so the chip can say WHICH model the child
/// runs on (`Agent · haiku`) without the reader opening the subagent tab.
///
/// Everything else — the prompt above all — stays in the host's run journal,
/// so this stays a whitelist of short identifiers rather than a size cap.
/// `None` for anything that is not a spawn, and for a spawn that named
/// neither, which keeps it idempotent: re-sanitizing a sanitized call is a
/// fixpoint (the kept keys are themselves kept).
fn spawn_badge(call: &ToolCall) -> Option<serde_json::Value> {
    if !call.is_subagent_spawn() {
        return None;
    }
    let input = match call {
        ToolCall::Unknown { input, .. } | ToolCall::Mcp { input, .. } => input.as_ref()?,
        _ => return None,
    };
    let kept: serde_json::Map<String, serde_json::Value> = SUBAGENT_INPUT_KEEP
        .iter()
        .filter_map(|key| {
            let value = input.get(key)?.as_str()?.trim();
            (!value.is_empty()).then(|| ((*key).to_owned(), serde_json::Value::from(value)))
        })
        .collect();
    (!kept.is_empty()).then(|| serde_json::Value::Object(kept))
}

/// Deterministic continuation id: `"{root}#c{n}"`.
pub fn continuation_id(root: &str, index: usize) -> String {
    format!("{root}#c{index}")
}

/// Split an oversized parts list into chunks each under `MSG_INLINE_MAX` bytes.
///
/// Splitting happens at part boundaries; an oversized text part is itself chunked at char
/// boundaries. Returns one Vec per resulting entry — the first keeps the root id, the rest are
/// continuations (`continuation_id(root, i)`), matching `splitMessageEntry` in luna.
pub fn split_parts(parts: &[MessagePart]) -> Vec<Vec<MessagePart>> {
    let mut chunks: Vec<Vec<MessagePart>> = vec![Vec::new()];
    let mut current_bytes = 0usize;

    let push_part = |chunks: &mut Vec<Vec<MessagePart>>, current: &mut usize, part: MessagePart| {
        let len = part.byte_len();
        if *current > 0 && *current + len > MSG_INLINE_MAX {
            chunks.push(Vec::new());
            *current = 0;
        }
        *current += len;
        chunks.last_mut().unwrap().push(part);
    };

    for part in parts {
        // Both text-bodied kinds chunk the same way; extended thinking can
        // exceed the cap just as easily as a long reply.
        let (id, text, reasoning) = match part {
            MessagePart::Text { id, text } if text.len() > MSG_INLINE_MAX => (id, text, false),
            MessagePart::Reasoning { id, text } if text.len() > MSG_INLINE_MAX => (id, text, true),
            other => {
                push_part(&mut chunks, &mut current_bytes, other.clone());
                continue;
            }
        };
        // Chunk oversized text at char boundaries.
        let mut start = 0usize;
        let mut piece = 0usize;
        while start < text.len() {
            let mut end = (start + MSG_INLINE_MAX).min(text.len());
            while end < text.len() && !text.is_char_boundary(end) {
                end -= 1;
            }
            // Guard: ensure forward progress on pathological boundaries.
            if end <= start {
                end = text.len();
            }
            let sub_id = if piece == 0 {
                id.clone()
            } else {
                format!("{id}~{piece}")
            };
            let body = text[start..end].to_string();
            let sub = if reasoning {
                MessagePart::Reasoning {
                    id: sub_id,
                    text: body,
                }
            } else {
                MessagePart::Text {
                    id: sub_id,
                    text: body,
                }
            };
            push_part(&mut chunks, &mut current_bytes, sub);
            start = end;
            piece += 1;
        }
    }
    chunks
}

/// Render-time inverse of splitting: concatenate continuation entries' parts in list order.
pub fn join_continuations(entries: Vec<Vec<MessagePart>>) -> Vec<MessagePart> {
    entries.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_use_unknown_refreshes_the_same_persisted_tool_part() {
        let call = |status, screenshot_observed| ToolCall::ComputerUse {
            action: "click".into(),
            app: Some("Finder".into()),
            task_id: Some("task-1".into()),
            thread_id: "provider-1".into(),
            turn_id: "turn-1".into(),
            status,
            screenshot_observed,
        };
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "item-1".into(),
                call: call(luna_proto::ComputerUseStatus::Running, false),
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "item-1".into(),
                call: call(luna_proto::ComputerUseStatus::Unknown, true),
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "item-1".into(),
                is_error: false,
                output: None,
                diff: None,
            },
        );
        assert!(matches!(
            parts.as_slice(),
            [MessagePart::Tool {
                call: ToolCall::ComputerUse {
                    status: luna_proto::ComputerUseStatus::Unknown,
                    screenshot_observed: true,
                    ..
                },
                resolved: true,
                ..
            }]
        ));
        assert_eq!(
            sanitize_tool_call(&call(luna_proto::ComputerUseStatus::Unknown, true)),
            call(luna_proto::ComputerUseStatus::Unknown, true)
        );
    }

    #[test]
    fn async_question_folds_once_without_becoming_pending_input() {
        let event = AgentEvent::AsyncQuestions {
            message_id: "call-1".into(),
            questions: vec![luna_proto::AsyncUserInputQuestion {
                title: "Which option?".into(),
                options: Some(vec!["A".into(), "B".into()]),
            }],
        };
        let mut parts = Vec::new();
        fold_event_into_parts(&mut parts, &event);
        fold_event_into_parts(&mut parts, &event);
        assert!(matches!(parts.as_slice(), [MessagePart::AsyncQuestion {
            id, resolved: false, ..
        }] if id == "aq-call-1"));
    }

    fn text_delta(s: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: s.into() }
    }

    #[test]
    fn reasoning_deltas_fold_into_their_own_part() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ReasoningDelta {
                text: "let me ".into(),
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ReasoningDelta {
                text: "think".into(),
            },
        );
        // Empty deltas (redacted-thinking heartbeats) never mint a part.
        fold_event_into_parts(&mut parts, &AgentEvent::ReasoningDelta { text: "".into() });
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0],
            MessagePart::Reasoning {
                id: "r0".into(),
                text: "let me think".into()
            }
        );
        // Text breaks the reasoning block; a later thought starts a new part.
        fold_event_into_parts(&mut parts, &text_delta("Answer"));
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ReasoningDelta {
                text: "more".into(),
            },
        );
        assert_eq!(parts.len(), 3);
        assert!(matches!(
            &parts[2],
            MessagePart::Reasoning { id, text } if id == "r2" && text == "more"
        ));
    }

    #[test]
    fn oversized_reasoning_splits_like_text() {
        let big = "x".repeat(MSG_INLINE_MAX + 10);
        let parts = vec![MessagePart::Reasoning {
            id: "r0".into(),
            text: big,
        }];
        let chunks = split_parts(&parts);
        let flat = join_continuations(chunks);
        assert!(flat.len() >= 2, "{}", flat.len());
        assert!(
            flat.iter()
                .all(|p| matches!(p, MessagePart::Reasoning { .. }))
        );
        assert!(flat.iter().all(|p| p.byte_len() <= MSG_INLINE_MAX));
        assert_eq!(flat[1].id(), "r0~1");
    }

    #[test]
    fn generated_image_fold_is_ordered_atomic_and_idempotent() {
        let mut parts = vec![];
        fold_event_into_parts(&mut parts, &text_delta("before"));
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "i".into(),
                call: ToolCall::Unknown {
                    name: "Generate image".into(),
                    input: None,
                },
            },
        );
        let event = AgentEvent::GeneratedImage {
            id: "i:image".into(),
            path: "/uploads/i.png".into(),
            name: "generated.png".into(),
            mime_type: "image/png".into(),
        };
        fold_event_into_parts(&mut parts, &event);
        fold_event_into_parts(&mut parts, &text_delta("after"));
        fold_event_into_parts(&mut parts, &event);
        assert_eq!(parts.len(), 4);
        assert!(matches!(&parts[2], MessagePart::Image { id, .. } if id == "i:image"));
        assert_eq!(join_continuations(split_parts(&parts)), parts);
    }

    #[test]
    fn text_deltas_merge_until_broken_by_tool() {
        let mut parts = Vec::new();
        fold_event_into_parts(&mut parts, &text_delta("Hello "));
        fold_event_into_parts(&mut parts, &text_delta("world"));
        assert_eq!(parts.len(), 1);
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "tool-1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        fold_event_into_parts(&mut parts, &text_delta("after"));
        assert_eq!(parts.len(), 3);
        match &parts[2] {
            MessagePart::Text { text, .. } => assert_eq!(text, "after"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn session_started_resets_accumulator() {
        let mut parts = Vec::new();
        fold_event_into_parts(&mut parts, &text_delta("junk"));
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::SessionStarted {
                harness: luna_proto::HarnessId::Mock,
                model: "m".into(),
                tools: vec![],
                cwd: "/".into(),
                session_id: "s".into(),
                assistant_message_id: "a".into(),
            },
        );
        assert!(parts.is_empty());
    }

    #[test]
    fn tool_call_refresh_is_idempotent() {
        let call = AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        };
        let mut once = Vec::new();
        fold_event_into_parts(&mut once, &call);
        let mut twice = once.clone();
        fold_event_into_parts(&mut twice, &call);
        assert_eq!(once, twice);
    }

    #[test]
    fn tool_result_marks_resolution() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t".into(),
                is_error: true,
                output: None,
                diff: None,
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                is_error, resolved, ..
            } => {
                assert!(*is_error);
                assert!(*resolved);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sanitize_strips_heavy_inputs_and_is_idempotent() {
        let call = ToolCall::WriteFile {
            path: "/x".into(),
            content: Some("secret".into()),
        };
        let clean = sanitize_tool_call(&call);
        assert_eq!(
            clean,
            ToolCall::WriteFile {
                path: "/x".into(),
                content: None
            }
        );
        assert_eq!(sanitize_tool_call(&clean), clean);
    }

    /// A spawn keeps the two short identifiers its chip names the child by and
    /// drops the prompt — the whole point of the whitelist. Still a fixpoint.
    #[test]
    fn sanitize_keeps_a_spawns_model_and_drops_its_prompt() {
        let call = ToolCall::Unknown {
            name: "Agent: Explore theme system".into(),
            input: Some(serde_json::json!({
                "description": "Explore theme system",
                "subagent_type": "Explore",
                "model": "haiku",
                "prompt": "a very long private prompt",
            })),
        };
        let clean = sanitize_tool_call(&call);
        assert_eq!(
            clean,
            ToolCall::Unknown {
                name: "Agent: Explore theme system".into(),
                input: Some(serde_json::json!({
                    "model": "haiku",
                    "subagent_type": "Explore",
                })),
            }
        );
        assert_eq!(clean.subagent_model(), Some("haiku"));
        assert_eq!(sanitize_tool_call(&clean), clean);
    }

    /// An ordinary tool's input still goes, even when it happens to carry a
    /// `model` argument — the badge is gated on the spawn genus, not the key.
    #[test]
    fn sanitize_still_strips_a_non_spawn_carrying_a_model_argument() {
        let call = ToolCall::Unknown {
            name: "SomeTool".into(),
            input: Some(serde_json::json!({ "model": "haiku", "prompt": "secret" })),
        };
        assert_eq!(
            sanitize_tool_call(&call),
            ToolCall::Unknown {
                name: "SomeTool".into(),
                input: None,
            }
        );
    }

    /// A spawn that named no model keeps no input at all — `None`, not an
    /// empty object, so the doc gains nothing and the fixpoint is exact.
    #[test]
    fn sanitize_drops_a_spawn_input_that_names_nothing_worth_keeping() {
        let call = ToolCall::Unknown {
            name: "Agent".into(),
            input: Some(serde_json::json!({ "prompt": "secret", "model": "  " })),
        };
        let clean = sanitize_tool_call(&call);
        assert_eq!(
            clean,
            ToolCall::Unknown {
                name: "Agent".into(),
                input: None,
            }
        );
        assert_eq!(clean.subagent_model(), None);
    }

    #[test]
    fn split_and_join_round_trip() {
        let big = "x".repeat(MSG_INLINE_MAX * 2 + 100);
        let parts = vec![
            MessagePart::Text {
                id: "t0".into(),
                text: big.clone(),
            },
            MessagePart::Tool {
                id: "tool-1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
                is_error: false,
                resolved: true,
                output: None,
                diff: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                diff_stats: None,
                subagent_ref: None,
                subagent_status: None,
                subagent_tail: None,
                subagent_activity: None,
            },
        ];
        let chunks = split_parts(&parts);
        assert!(
            chunks.len() >= 3,
            "expected >=3 chunks, got {}",
            chunks.len()
        );
        for chunk in &chunks {
            let bytes: usize = chunk.iter().map(|p| p.byte_len()).sum();
            assert!(bytes <= MSG_INLINE_MAX, "chunk over cap: {bytes}");
        }
        let joined = join_continuations(chunks);
        let text: String = joined
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, big);
        assert!(matches!(joined.last().unwrap(), MessagePart::Tool { .. }));
    }

    #[test]
    fn continuation_ids_are_deterministic() {
        assert_eq!(continuation_id("m1", 1), "m1#c1");
    }

    // ── A1 strip (docs/chat2-sync.md) ───────────────────────────────────────

    #[test]
    fn summarize_inlines_small_outputs_and_marks_big_cuts() {
        assert_eq!(summarize_tool_output(""), None);
        assert_eq!(summarize_tool_output("  \n\t\n"), None);
        assert_eq!(summarize_tool_output("one line"), Some("one line".into()));
        // Small multi-line outputs ride whole — no summary, no "…".
        assert_eq!(
            summarize_tool_output("\n\nfirst real\nsecond"),
            Some("first real\nsecond".into())
        );
        assert_eq!(
            summarize_tool_output("only line\n\n  \n"),
            Some("only line".into())
        );
        // Markdown fences are transport wrapping, never content: stripped
        // even when they'd otherwise be the first line, and a fence-only
        // output is blank.
        assert_eq!(
            summarize_tool_output("```console\nreal content\n```"),
            Some("real content".into())
        );
        assert_eq!(summarize_tool_output("```\n```"), None);
        // Big outputs: first non-empty (post-fence) line + unconditional "…".
        let big = format!("```console\nhead line\n{}\n```", "x".repeat(300));
        assert_eq!(summarize_tool_output(&big), Some("head line…".into()));
        let long = "x".repeat(TOOL_OUTPUT_SUMMARY_MAX + 40);
        let summary = summarize_tool_output(&long).unwrap();
        assert_eq!(summary.chars().count(), TOOL_OUTPUT_SUMMARY_MAX + 1);
        assert!(summary.ends_with('…'));
        // Char-boundary safety on multibyte input.
        let wide = "é".repeat(TOOL_OUTPUT_SUMMARY_MAX + 5);
        let summary = summarize_tool_output(&wide).unwrap();
        assert_eq!(summary.chars().count(), TOOL_OUTPUT_SUMMARY_MAX + 1);
    }

    #[test]
    fn diff_stat_counts_line_changes() {
        let stat = diff_stat(&ToolDiff {
            path: "/w/a.rs".into(),
            old_text: Some("a\nb\nc\n".into()),
            new_text: "a\nB\nc\nd\n".into(),
            unified_diff: None,
        });
        assert_eq!(stat.path, "/w/a.rs");
        assert_eq!(stat.additions, 2); // B + d
        assert_eq!(stat.deletions, 1); // b
        // New file: every line is an addition.
        let stat = diff_stat(&ToolDiff {
            path: "/w/new.rs".into(),
            old_text: None,
            new_text: "one\ntwo\n".into(),
            unified_diff: None,
        });
        assert_eq!((stat.additions, stat.deletions), (2, 0));
    }

    #[test]
    fn fold_strips_output_to_summary_and_diff_to_stats() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t".into(),
                call: ToolCall::Exec {
                    command: "cargo test".into(),
                },
            },
        );
        let full = "running 42 tests\n".repeat(300); // ~5KB, was 4KB inline pre-strip
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t".into(),
                is_error: false,
                output: Some(full.clone()),
                diff: Some(ToolDiff {
                    path: "/w/a.rs".into(),
                    old_text: Some("a\n".into()),
                    new_text: "b\n".into(),
                    unified_diff: None,
                }),
            },
        );
        apply_sidecar_refs("chat-9", &mut parts);
        match &parts[0] {
            MessagePart::Tool {
                output,
                output_ref,
                output_bytes,
                diff,
                diff_stats,
                ..
            } => {
                assert_eq!(output.as_deref(), Some("running 42 tests…"));
                assert_eq!(output_ref.as_deref(), Some("chat-9/t"));
                assert_eq!(*output_bytes, Some(full.len() as u64));
                assert!(diff.is_none(), "inline diff text must not enter the doc");
                let stats = diff_stats.as_ref().unwrap();
                assert_eq!(stats.len(), 1);
                assert_eq!((stats[0].additions, stats[0].deletions), (1, 1));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sidecar_refs_stamp_once_and_only_where_content_exists() {
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "t1".into(),
                call: ToolCall::Exec {
                    command: "ls".into(),
                },
            },
        );
        // Unresolved: no refs yet.
        apply_sidecar_refs("chat-9", &mut parts);
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                output_ref: None,
                diff_ref: None,
                ..
            }
        ));
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "t1".into(),
                is_error: false,
                output: Some("hello".into()),
                diff: Some(ToolDiff {
                    path: "/w/a".into(),
                    old_text: None,
                    new_text: "x\n".into(),
                    unified_diff: None,
                }),
            },
        );
        apply_sidecar_refs("chat-9", &mut parts);
        apply_sidecar_refs("chat-9", &mut parts); // idempotent
        match &parts[0] {
            MessagePart::Tool {
                output,
                output_ref,
                output_bytes,
                diff_ref,
                ..
            } => {
                assert_eq!(output.as_deref(), Some("hello"));
                assert_eq!(output_ref.as_deref(), None);
                assert_eq!(*output_bytes, None);
                assert_eq!(diff_ref.as_deref(), Some("chat-9/t1.diff"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn subagent_events_refresh_the_spawn_chip_in_place() {
        use luna_proto::DoneStatus;
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "toolu_sub".into(),
                call: ToolCall::Unknown {
                    name: "Agent".into(),
                    input: None,
                },
            },
        );
        // Tagged traffic marks the chip running — and ONLY that: the tail
        // stays untouched (per-delta chip rewrites polluted the parent doc).
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::Subagent {
                parent_tool_use_id: "toolu_sub".into(),
                event: Box::new(AgentEvent::TextDelta {
                    text: "scanning\nfound 3 issues".into(),
                }),
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                subagent_status,
                subagent_tail,
                ..
            } => {
                assert_eq!(*subagent_status, Some(SubagentStatus::Running));
                assert_eq!(*subagent_tail, None);
            }
            other => panic!("{other:?}"),
        }
        // A tagged Done is terminal; later traffic must not regress it.
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::Subagent {
                parent_tool_use_id: "toolu_sub".into(),
                event: Box::new(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                }),
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::Subagent {
                parent_tool_use_id: "toolu_sub".into(),
                event: Box::new(AgentEvent::TextDelta {
                    text: "late flush".into(),
                }),
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                subagent_status, ..
            } => assert_eq!(*subagent_status, Some(SubagentStatus::Done)),
            other => panic!("{other:?}"),
        }
        // A confirmed follow-up can reopen the same chip without user text.
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::Subagent {
                parent_tool_use_id: "toolu_sub".into(),
                event: Box::new(AgentEvent::Steered {
                    user_message_id: None,
                    assistant_message_id: None,
                    next_assistant_message_id: None,
                }),
            },
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                subagent_status: Some(SubagentStatus::Running),
                ..
            }
        ));
        // Content never leaked into the parent parts.
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn child_observations_keep_waiting_usage_result_and_reactivation_distinct() {
        use luna_proto::DoneStatus;
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "spawn".into(),
                call: ToolCall::Unknown {
                    name: "Agent: scan".into(),
                    input: None,
                },
            },
        );
        let tagged = |event| AgentEvent::Subagent {
            parent_tool_use_id: "spawn".into(),
            event: Box::new(event),
        };
        // The child can finish before the parent spawn tool resolves.
        fold_event_into_parts(
            &mut parts,
            &tagged(AgentEvent::InputRequested {
                request_id: "approval".into(),
                questions: Vec::new(),
            }),
        );
        fold_event_into_parts(
            &mut parts,
            &tagged(AgentEvent::Usage {
                input_tokens: 12,
                output_tokens: 0,
            }),
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                subagent_status: Some(SubagentStatus::Waiting),
                subagent_activity: Some(SubagentActivity {
                    input_tokens: Some(12),
                    output_tokens: Some(0),
                    ..
                }),
                ..
            }
        ));
        fold_event_into_parts(
            &mut parts,
            &tagged(AgentEvent::InputResolved {
                request_id: "approval".into(),
            }),
        );
        fold_event_into_parts(
            &mut parts,
            &tagged(AgentEvent::ChildStatus {
                status: luna_proto::ChildRuntimeStatus::Idle,
            }),
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                subagent_status: Some(SubagentStatus::Idle),
                ..
            }
        ));
        fold_event_into_parts(
            &mut parts,
            &tagged(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: Some("Found two fixes".into()),
                error: None,
                session_id: None,
            }),
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolResult {
                id: "spawn".into(),
                is_error: false,
                output: Some("summary".into()),
                diff: None,
            },
        );
        assert!(matches!(&parts[0], MessagePart::Tool {
            subagent_status: Some(SubagentStatus::Done),
            subagent_activity: Some(SubagentActivity { result: Some(result), .. }),
            ..
        } if result == "Found two fixes"));
        fold_event_into_parts(
            &mut parts,
            &tagged(AgentEvent::Steered {
                user_message_id: None,
                assistant_message_id: None,
                next_assistant_message_id: None,
            }),
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Tool {
                subagent_status: Some(SubagentStatus::Running),
                subagent_activity: None,
                ..
            }
        ));
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn subagent_events_never_decorate_a_non_spawn_chip() {
        // Mis-keyed tagged traffic (claude's background shells settled
        // through the subagent subtype, 2026-08-20) must not stamp lifecycle
        // onto an ordinary tool chip — the genus gate is the CALL.
        use luna_proto::DoneStatus;
        let mut parts = Vec::new();
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::ToolCall {
                id: "toolu_bash".into(),
                call: ToolCall::Exec {
                    command: "git clone …".into(),
                },
            },
        );
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::Subagent {
                parent_tool_use_id: "toolu_bash".into(),
                event: Box::new(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                }),
            },
        );
        match &parts[0] {
            MessagePart::Tool {
                subagent_status, ..
            } => assert_eq!(*subagent_status, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sidecar_payload_carries_full_texts() {
        assert_eq!(
            sidecar_payload(&AgentEvent::TextDelta { text: "x".into() }),
            None
        );
        assert_eq!(
            sidecar_payload(&AgentEvent::ToolResult {
                id: "t".into(),
                is_error: false,
                output: Some("   \n".into()),
                diff: None,
            }),
            None,
            "blank output uploads nothing"
        );
        let payload = sidecar_payload(&AgentEvent::ToolResult {
            id: "t".into(),
            is_error: true,
            output: Some("full output".into()),
            diff: None,
        })
        .unwrap();
        assert_eq!(payload.part_id, "t");
        assert_eq!(payload.output.as_deref(), Some("full output"));
    }
}
