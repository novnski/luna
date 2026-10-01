//! CC-02 — typed, tolerant decoding of the Claude Code stream-json frames that
//! the narrow wire enum does not cover, and bounded, redacted retention of the
//! rest.
//!
//! Shapes follow the SDK schema embedded in CLI 2.1.286 (`SDKResultMessage`,
//! `SDKPartialAssistantMessage`, `SDKAssistantMessage`, `MessageOrigin`,
//! `conversation_reset`, `command_lifecycle`, and the task / hook / compaction
//! system subtypes) plus the checked-in live 2.1.228 capture.
//!
//! Every decoder here is per-field tolerant: a field of an unexpected type is
//! dropped (results also name it in `invalid_fields`), never fatal to the frame.
//! A lost `result` frame strands a warm turn in Working, so the result decoder
//! must always produce an outcome.
//!
//! Nothing in this module is a transcript part, a turn boundary or an
//! authority. Decoded lifecycle frames and unknown frames reach the host only
//! as [`AgentEvent::ProviderEvidence`] (journal-only) through
//! [`super::turns::NativeTracker`].

use std::collections::BTreeMap;

use luna_proto::{AgentEvent, HarnessId};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// The CLI whose embedded SDK schema documents client-uuid echo
/// (`user_message_uuid(s)`) on stream, assistant and result frames. Older
/// runtimes keep the legacy completion rule until an echo is observed.
pub(crate) const CERTIFIED_ECHO_VERSION: [u64; 3] = [2, 1, 286];

const MAX_ID_BYTES: usize = 256;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_HOOK_OUTPUT_BYTES: usize = 4 * 1024;
const MAX_ERROR_BYTES: usize = 2 * 1024;
const MAX_ECHOED_UUIDS: usize = 64;
const MAX_LIST_ITEMS: usize = 64;
const MAX_SAMPLE_BYTES: usize = 8 * 1024;
const MAX_SAMPLE_STRING_BYTES: usize = 512;
const MAX_SAMPLE_DEPTH: usize = 6;
const MAX_SAMPLE_ITEMS: usize = 32;
const MAX_NON_JSON_SAMPLE_BYTES: usize = 256;

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// A bounded identifier: non-empty, at most 256 bytes, no control characters.
fn ident_str(raw: &str) -> Option<String> {
    (!raw.is_empty() && raw.len() <= MAX_ID_BYTES && !raw.chars().any(char::is_control))
        .then(|| raw.to_owned())
}

fn ident(value: &Value, key: &str) -> Option<String> {
    ident_str(str_of(value, key)?)
}

fn label(value: &Value, key: &str) -> Option<String> {
    str_of(value, key).map(|text| bounded(text, MAX_LABEL_BYTES))
}

fn uint(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn flag(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

/// Cut `text` to at most `limit` bytes on a char boundary, marking the cut.
pub(crate) fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated {} bytes]", &text[..end], text.len() - end)
}

/// `true` when a `major.minor.patch` version string is at least `min`.
/// Anything unparseable is treated as older (fail closed to legacy rules).
pub(crate) fn version_at_least(version: &str, min: [u64; 3]) -> bool {
    let mut numbers = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().ok());
    let mut parsed = [0u64; 3];
    for slot in &mut parsed {
        match numbers.next() {
            Some(Some(number)) => *slot = number,
            _ => return false,
        }
    }
    parsed >= min
}

// ── Turn identity ──────────────────────────────────────────────────────────

/// Provenance of a user-role turn (`MessageOrigin`). An open set: an unknown
/// future kind is kept verbatim and is never treated as human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageOrigin {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subkind: Option<String>,
    /// Sender-asserted address (`peer.from`, `observer.from`, `channel.server`).
    /// Display/provenance only, never authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl MessageOrigin {
    pub fn decode(value: Option<&Value>) -> Option<Self> {
        let value = value?;
        let kind = ident(value, "kind")?;
        Some(Self {
            kind,
            subkind: ident(value, "subkind"),
            from: ident(value, "from").or_else(|| ident(value, "server")),
        })
    }

    pub fn is_human(&self) -> bool {
        self.kind == "human"
    }
}

/// The turn-identity fields a frame envelope may carry: the frame's own
/// native `uuid`, the client uuid(s) it answers, its origin, and whether it
/// is replayed history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TurnStamp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_uuid: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub user_message_uuids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub historical: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_reason: Option<String>,
}

impl TurnStamp {
    pub fn decode(value: &Value) -> Self {
        let user_message_uuids = value
            .get("user_message_uuids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(ident_str)
                    .take(MAX_ECHOED_UUIDS)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            native_uuid: ident(value, "uuid"),
            user_message_uuid: ident(value, "user_message_uuid"),
            user_message_uuids,
            origin: MessageOrigin::decode(value.get("origin")),
            historical: flag(value, "historical").unwrap_or(false),
            resume_reason: label(value, "resume_reason"),
        }
    }

    /// Every client uuid this frame echoes, consumption order first, deduped.
    pub fn echoed(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for uuid in self
            .user_message_uuids
            .iter()
            .chain(self.user_message_uuid.iter())
        {
            if !out.contains(&uuid.as_str()) {
                out.push(uuid);
            }
        }
        out
    }
}

// ── Result envelope ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResultUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

/// Every documented `result` field LUNA can use, decoded per field.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResultEnvelope {
    pub subtype: String,
    pub is_error: bool,
    /// Final assistant text (success) or the API error text (`is_error`).
    /// Carried on `Done`, never duplicated into evidence (length only).
    #[serde(skip)]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    pub usage: ResultUsage,
    #[serde(skip)]
    pub model_usage: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_turns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_api_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<i64>,
    pub permission_denials: usize,
    pub has_structured_output: bool,
    #[serde(skip)]
    #[allow(dead_code)] // Structured-output projection belongs to CC-05.
    pub structured_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_turn_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub stamp: TurnStamp,
    /// Documented fields that were present with an unexpected type.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalid_fields: Vec<&'static str>,
}

impl ResultEnvelope {
    pub fn decode(value: &Value) -> Self {
        let mut invalid = Vec::new();
        let mut check = |key: &'static str, ok: fn(&Value) -> bool| {
            if value
                .get(key)
                .is_some_and(|field| !field.is_null() && !ok(field))
            {
                invalid.push(key);
            }
        };
        check("subtype", Value::is_string);
        check("result", Value::is_string);
        check("errors", Value::is_array);
        check("usage", Value::is_object);
        check("modelUsage", Value::is_object);
        check("session_id", Value::is_string);
        check("total_cost_usd", Value::is_number);
        check("is_error", Value::is_boolean);
        let usage = value.get("usage").filter(|usage| usage.is_object());
        let usage = ResultUsage {
            input_tokens: usage.and_then(|u| uint(u, "input_tokens")).unwrap_or(0),
            output_tokens: usage.and_then(|u| uint(u, "output_tokens")).unwrap_or(0),
            cache_read_input_tokens: usage.and_then(|u| uint(u, "cache_read_input_tokens")),
            cache_creation_input_tokens: usage.and_then(|u| uint(u, "cache_creation_input_tokens")),
        };
        let model_usage: BTreeMap<String, Value> = value
            .get("modelUsage")
            .and_then(Value::as_object)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(name, entry)| entry.is_object() && ident_str(name).is_some())
                    .take(MAX_LIST_ITEMS)
                    .map(|(name, entry)| (name.clone(), entry.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let errors = value
            .get("errors")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(MAX_LIST_ITEMS)
                    .map(|item| match item {
                        Value::String(text) => bounded(text, MAX_ERROR_BYTES),
                        other => bounded(&other.to_string(), MAX_ERROR_BYTES),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let result = str_of(value, "result").map(str::to_owned);
        let structured_output = value
            .get("structured_output")
            .filter(|v| !v.is_null())
            .cloned();
        Self {
            subtype: str_of(value, "subtype").unwrap_or_default().to_owned(),
            is_error: flag(value, "is_error").unwrap_or(false),
            result_bytes: result.as_ref().map(String::len),
            result,
            errors,
            usage,
            models: model_usage.keys().cloned().collect(),
            model_usage,
            total_cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
            num_turns: uint(value, "num_turns"),
            duration_ms: uint(value, "duration_ms"),
            duration_api_ms: uint(value, "duration_api_ms"),
            stop_reason: label(value, "stop_reason"),
            terminal_reason: label(value, "terminal_reason"),
            api_error_status: value.get("api_error_status").and_then(Value::as_i64),
            permission_denials: value
                .get("permission_denials")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
            has_structured_output: structured_output.is_some(),
            structured_output,
            local_command: label(value, "local_command"),
            result_index: uint(value, "result_index"),
            queued_turn_count: uint(value, "queued_turn_count"),
            session_id: ident(value, "session_id"),
            stamp: TurnStamp::decode(value),
            invalid_fields: invalid,
        }
    }
}

// ── Tool-result bodies (G2 foundation) ─────────────────────────────────────

/// The text body of a `tool_result` block's `content`: a string, or an array
/// of content blocks in order. Text blocks stay verbatim; images, documents
/// and other rich blocks become one bounded placeholder line each (never
/// base64). `None` = the field is absent (unavailable); `Some("")` = empty.
pub(crate) fn tool_result_text(content: &Value) -> Option<String> {
    match content {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .map(tool_result_block_text)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        other => Some(format!(
            "[unsupported tool_result content: {}]",
            json_kind(other)
        )),
    }
}

fn tool_result_block_text(block: &Value) -> String {
    let kind = str_of(block, "type").unwrap_or("");
    match kind {
        "text" => str_of(block, "text").unwrap_or_default().to_owned(),
        "image" => {
            let source = block.get("source").cloned().unwrap_or(Value::Null);
            let media = label(&source, "media_type").unwrap_or_else(|| "image".into());
            match str_of(&source, "data") {
                Some(data) => format!("[image: {media}, {} base64 bytes]", data.len()),
                None => match label(&source, "url") {
                    Some(url) => format!("[image: {media}, {url}]"),
                    None => format!("[image: {media}]"),
                },
            }
        }
        "document" => {
            let name = label(block, "title")
                .or_else(|| block.get("source").and_then(|s| label(s, "media_type")))
                .unwrap_or_else(|| "document".into());
            format!("[document: {name}]")
        }
        "tool_reference" => format!(
            "[tool reference: {}]",
            label(block, "tool_name").unwrap_or_default()
        ),
        "search_result" => format!(
            "[search result: {} {}]",
            label(block, "title").unwrap_or_default(),
            label(block, "source").unwrap_or_default()
        ),
        "resource" | "resource_link" => format!(
            "[resource: {}]",
            label(block, "uri")
                .or_else(|| block.get("resource").and_then(|r| label(r, "uri")))
                .unwrap_or_default()
        ),
        other => format!(
            "[unsupported tool_result block: {}]",
            bounded(if other.is_empty() { "untyped" } else { other }, 64)
        ),
    }
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ── Lifecycle families ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskUsage {
    pub total_tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackgroundTask {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `task_started` / `task_progress` / `task_updated` / `task_notification` /
/// `task_summary` / `background_tasks_changed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskEvent {
    pub phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_backgrounded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by_subagent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_depth: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TaskUsage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<BackgroundTask>,
}

impl TaskEvent {
    fn decode(phase: &'static str, value: &Value) -> Self {
        // `task_updated` carries its status/description/error in `patch`.
        let patch = value.get("patch").filter(|p| p.is_object());
        let field = |key: &str| {
            patch
                .and_then(|p| label(p, key))
                .or_else(|| label(value, key))
        };
        let usage = value
            .get("usage")
            .filter(|u| u.is_object())
            .map(|u| TaskUsage {
                total_tokens: uint(u, "total_tokens"),
                tool_uses: uint(u, "tool_uses"),
                duration_ms: uint(u, "duration_ms"),
            });
        let tasks = value
            .get("tasks")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(BackgroundTask {
                            task_id: ident(item, "task_id")?,
                            task_type: label(item, "task_type"),
                            description: label(item, "description"),
                        })
                    })
                    .take(MAX_LIST_ITEMS)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            phase,
            task_id: ident(value, "task_id"),
            tool_use_id: ident(value, "tool_use_id"),
            description: field("description"),
            subagent_type: label(value, "subagent_type"),
            task_type: label(value, "task_type"),
            workflow_name: label(value, "workflow_name"),
            is_backgrounded: patch
                .and_then(|p| flag(p, "is_backgrounded"))
                .or_else(|| flag(value, "is_backgrounded")),
            owned_by_subagent: flag(value, "owned_by_subagent"),
            spawn_depth: uint(value, "spawn_depth"),
            prompt_bytes: str_of(value, "prompt").map(str::len),
            status: field("status"),
            reason: label(value, "reason"),
            summary: field("summary").or_else(|| label(value, "detail")),
            output_file: label(value, "output_file"),
            last_tool_name: label(value, "last_tool_name"),
            error: field("error"),
            usage,
            tasks,
        }
    }
}

/// `hook_started` / `hook_progress` / `hook_response`. Outputs are bounded.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookEvent {
    pub phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

impl HookEvent {
    fn decode(phase: &'static str, value: &Value) -> Self {
        let output = |key: &str| {
            str_of(value, key)
                .filter(|text| !text.is_empty())
                .map(|text| bounded(text, MAX_HOOK_OUTPUT_BYTES))
        };
        Self {
            phase,
            hook_id: ident(value, "hook_id"),
            hook_name: label(value, "hook_name"),
            hook_event: label(value, "hook_event"),
            outcome: label(value, "outcome"),
            exit_code: value.get("exit_code").and_then(Value::as_i64),
            output: output("output"),
            stdout: output("stdout"),
            stderr: output("stderr"),
        }
    }
}

/// `status` (`compacting` / `compact_result` / `compact_error`) and
/// `compact_boundary`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompactionEvent {
    pub phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_parent_uuid: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub historical: bool,
}

/// `conversation_reset` (`/clear`, plan-mode exit, fresh session, onboarding).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// `command_lifecycle` — the fate of one client-uuid send
/// (`queued`/`started`/`completed`/`cancelled`/`discarded`/`refused`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandLifecycle {
    pub command_uuid: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "family", rename_all = "camelCase")]
pub(crate) enum NativeEvent {
    Task(TaskEvent),
    Hook(HookEvent),
    Compaction(CompactionEvent),
    ConversationReset(ResetEvent),
    CommandLifecycle(CommandLifecycle),
}

/// Readback from `system:init` that the turn ledger needs.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InitInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_code_version: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_style: Option<String>,
}

/// Bounded, redacted copy of a frame LUNA does not project.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSample {
    /// Byte length of the raw stdout line.
    pub bytes: usize,
    /// SHA-256 of the raw line, to correlate with a raw capture.
    pub sha256: String,
    pub sample: Value,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NativeBody {
    /// A decoded lifecycle frame.
    Event(NativeEvent),
    /// `system:init` readback.
    Init(InitInfo),
    /// Known non-semantic progress: counted, never retained per frame.
    Progress,
    /// Known semantic frame LUNA does not project yet: retained sample.
    Unprojected(RawSample),
    /// Unknown type/subtype: retained sample.
    Unknown(RawSample),
}

/// One classified frame (`kind` is `type` or `system/<subtype>`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeFrame {
    pub kind: String,
    pub native_uuid: Option<String>,
    pub body: NativeBody,
}

/// System subtypes that are pure progress / UI chrome for LUNA's purposes.
const PROGRESS_SYSTEM_SUBTYPES: &[&str] = &[
    "thinking_tokens",
    "thinking",
    "session_state_changed",
    "turn_duration",
    "control_request_progress",
    "cloud_session_delta",
    "per_turn_effort_changed",
    "session_title_changed",
    "session_metadata",
    "vcs_state_changed",
    "ui_focus",
    "ui_invalidate",
    "ui_log",
    "ui_panes",
    "ui_scroll",
    "ui_status",
    "ui_toast",
];

/// Every other system subtype the 2.1.286 schema documents. Frames of these
/// subtypes are retained as `native:` samples; anything else is `unknown:`.
const DOCUMENTED_SYSTEM_SUBTYPES: &[&str] = &[
    "agents_killed",
    "api_error",
    "api_retry",
    "away_summary",
    "code_change_published",
    "commands_changed",
    "dev_intent",
    "elicitation_complete",
    "feedback_draft_queued",
    "file_snapshot",
    "files_persisted",
    "informational",
    "local_command_output",
    "memory_recall",
    "memory_saved",
    "mirror_error",
    "model_consent_fallback",
    "model_fallback",
    "model_refusal_fallback",
    "model_refusal_no_fallback",
    "notification",
    "peer_message_hold",
    "permission_denied",
    "permission_retry",
    "plugin_install",
    "post_turn_summary",
    "scheduled_task_fire",
    "stop_hook_summary",
    "turn_handoff_available",
    "turn_preempted",
    "upgrade_relay_marker",
    "worker_shutting_down",
];

/// Top-level frame types that are pure progress for LUNA.
const PROGRESS_TYPES: &[&str] = &["keep_alive", "tool_progress", "stream_mode"];

/// Top-level frame types the schema documents but LUNA does not project yet.
const DOCUMENTED_TYPES: &[&str] = &[
    "auth_status",
    "prompt_suggestion",
    "tool_use_summary",
    "transcript_mirror",
    "queued_notification",
    "session_notice",
    "api_metrics",
];

/// Classify a `system` frame (other than the fields `SystemFrame` already
/// decodes for the established normalizer).
pub(crate) fn decode_system(subtype: &str, value: &Value, line: &str) -> NativeFrame {
    let kind = format!("system/{}", bounded(subtype, 64));
    let native_uuid = ident(value, "uuid");
    let body = match subtype {
        "init" => NativeBody::Init(InitInfo {
            claude_code_version: label(value, "claude_code_version"),
            capabilities: value
                .get("capabilities")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .filter_map(ident_str)
                        .take(MAX_LIST_ITEMS)
                        .collect()
                })
                .unwrap_or_default(),
            permission_mode: label(value, "permissionMode"),
            output_style: label(value, "output_style"),
        }),
        "task_started"
        | "task_progress"
        | "task_updated"
        | "task_notification"
        | "task_summary"
        | "background_tasks_changed" => {
            let phase = match subtype {
                "task_started" => "started",
                "task_progress" => "progress",
                "task_updated" => "updated",
                "task_notification" => "notification",
                "task_summary" => "summary",
                _ => "backgroundChanged",
            };
            NativeBody::Event(NativeEvent::Task(TaskEvent::decode(phase, value)))
        }
        "hook_started" | "hook_progress" | "hook_response" => {
            let phase = match subtype {
                "hook_started" => "started",
                "hook_progress" => "progress",
                _ => "response",
            };
            NativeBody::Event(NativeEvent::Hook(HookEvent::decode(phase, value)))
        }
        "compact_boundary" => {
            let meta = value
                .get("compact_metadata")
                .cloned()
                .unwrap_or(Value::Null);
            NativeBody::Event(NativeEvent::Compaction(CompactionEvent {
                phase: "boundary",
                trigger: label(&meta, "trigger"),
                pre_tokens: uint(&meta, "pre_tokens"),
                post_tokens: uint(&meta, "post_tokens"),
                duration_ms: uint(&meta, "duration_ms"),
                result: None,
                error: None,
                logical_parent_uuid: ident(value, "logical_parent_uuid"),
                historical: flag(value, "historical").unwrap_or(false),
            }))
        }
        "status" => {
            let compacting = str_of(value, "status") == Some("compacting");
            let result = label(value, "compact_result");
            let error = str_of(value, "compact_error").map(|e| bounded(e, MAX_ERROR_BYTES));
            if compacting || result.is_some() || error.is_some() {
                NativeBody::Event(NativeEvent::Compaction(CompactionEvent {
                    phase: if compacting { "started" } else { "finished" },
                    trigger: None,
                    pre_tokens: None,
                    post_tokens: None,
                    duration_ms: None,
                    result,
                    error,
                    logical_parent_uuid: None,
                    historical: false,
                }))
            } else {
                NativeBody::Progress
            }
        }
        known if PROGRESS_SYSTEM_SUBTYPES.contains(&known) => NativeBody::Progress,
        known if DOCUMENTED_SYSTEM_SUBTYPES.contains(&known) => {
            NativeBody::Unprojected(raw_sample(line, Some(value)))
        }
        _ => NativeBody::Unknown(raw_sample(line, Some(value))),
    };
    NativeFrame {
        kind,
        native_uuid,
        body,
    }
}

/// Classify a top-level frame type the wire enum does not route.
pub(crate) fn decode_top_level(kind: &str, value: &Value, line: &str) -> NativeFrame {
    let native_uuid = ident(value, "uuid");
    let body = match kind {
        "conversation_reset" => NativeBody::Event(NativeEvent::ConversationReset(ResetEvent {
            new_conversation_id: ident(value, "new_conversation_id"),
            trigger: label(value, "trigger"),
            user_message_uuid: ident(value, "user_message_uuid"),
            session_id: ident(value, "session_id"),
        })),
        "command_lifecycle" => match (
            ident(value, "command_uuid").or_else(|| ident(value, "uuid")),
            label(value, "state"),
        ) {
            (Some(command_uuid), Some(state)) => {
                NativeBody::Event(NativeEvent::CommandLifecycle(CommandLifecycle {
                    command_uuid,
                    state,
                }))
            }
            _ => NativeBody::Unknown(raw_sample(line, Some(value))),
        },
        known if PROGRESS_TYPES.contains(&known) => NativeBody::Progress,
        known if DOCUMENTED_TYPES.contains(&known) => {
            NativeBody::Unprojected(raw_sample(line, Some(value)))
        }
        _ => NativeBody::Unknown(raw_sample(line, Some(value))),
    };
    NativeFrame {
        kind: if kind.is_empty() {
            "untyped".into()
        } else {
            bounded(kind, 64)
        },
        native_uuid,
        body,
    }
}

// ── Bounded, redacted samples ──────────────────────────────────────────────

const SENSITIVE_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "auth",
    "cookie",
    "credential",
    "api_key",
    "apikey",
    "private_key",
    "session_key",
    "bearer",
    "signature",
];

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SENSITIVE_KEY_PARTS.iter().any(|part| key.contains(part))
}

/// A bounded, redacted copy of `line` (parsed as `value` when JSON). String,
/// array and object values under credential-like keys become `[redacted]`
/// (numbers and booleans such as token counts stay). Depth, item and byte
/// budgets bound the copy; `truncated` records any cut.
pub(crate) fn raw_sample(line: &str, value: Option<&Value>) -> RawSample {
    let sha256 = Sha256::digest(line.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut truncated = false;
    let sample = match value {
        Some(value) => {
            let mut budget = MAX_SAMPLE_BYTES;
            redact(value, 0, &mut budget, &mut truncated)
        }
        None => {
            truncated = line.len() > MAX_NON_JSON_SAMPLE_BYTES;
            Value::String(bounded(line, MAX_NON_JSON_SAMPLE_BYTES))
        }
    };
    RawSample {
        bytes: line.len(),
        sha256,
        sample,
        truncated,
    }
}

fn redact(value: &Value, depth: usize, budget: &mut usize, truncated: &mut bool) -> Value {
    if *budget == 0 || depth > MAX_SAMPLE_DEPTH {
        *truncated = true;
        return Value::String("[…]".into());
    }
    match value {
        Value::String(text) => {
            let kept = bounded(text, MAX_SAMPLE_STRING_BYTES.min(*budget));
            if kept.len() != text.len() {
                *truncated = true;
            }
            *budget = budget.saturating_sub(kept.len());
            Value::String(kept)
        }
        Value::Array(items) => {
            if items.len() > MAX_SAMPLE_ITEMS {
                *truncated = true;
            }
            Value::Array(
                items
                    .iter()
                    .take(MAX_SAMPLE_ITEMS)
                    .map(|item| redact(item, depth + 1, budget, truncated))
                    .collect(),
            )
        }
        Value::Object(map) => {
            if map.len() > MAX_SAMPLE_ITEMS {
                *truncated = true;
            }
            let mut out = Map::new();
            for (key, field) in map.iter().take(MAX_SAMPLE_ITEMS) {
                *budget = budget.saturating_sub(key.len());
                let redacted = sensitive_key(key)
                    && matches!(field, Value::String(_) | Value::Array(_) | Value::Object(_));
                let copy = if redacted {
                    Value::String("[redacted]".into())
                } else {
                    redact(field, depth + 1, budget, truncated)
                };
                out.insert(bounded(key, 128), copy);
            }
            Value::Object(out)
        }
        other => {
            *budget = budget.saturating_sub(8);
            other.clone()
        }
    }
}

// ── Evidence ───────────────────────────────────────────────────────────────

/// Host-only evidence for one decoded or retained frame.
pub(crate) fn evidence(
    kind: impl Into<String>,
    native_id: Option<String>,
    payload: Value,
) -> AgentEvent {
    AgentEvent::ProviderEvidence {
        provider: HarnessId::ClaudeCode,
        kind: kind.into(),
        native_id,
        payload,
    }
}

pub(crate) fn to_payload<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|error| json!({ "encodeError": error.to_string() }))
}
