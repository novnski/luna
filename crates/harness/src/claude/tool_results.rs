//! Claude tool-result projection: complete native result bodies, native diffs,
//! lossless arguments and typed assets, in the host-owned `lunaToolOutput`
//! envelope the shared journal, lazy fetch and output viewer already carry.
//!
//! Source of the shapes: the Claude Code 2.1.286 SDK schema and emitter.
//! - A `tool_result` block's `content` is a string or Anthropic content blocks
//!   (`text`, `image` with a base64/url `source`, `document`, `search_result`, …).
//! - The user frame's `tool_use_result` is the tool's full Output object
//!   (Bash `stdout/stderr/...`, Edit/Write `structuredPatch/originalFile`,
//!   Read `file{...}`), or `{content, _meta?, structuredContent?, resourceLinks?}`
//!   for an MCP call that returned protocol metadata.
//! - `tool_result_meta` carries `non_execution_kind`/`user_feedback`/`remedy`
//!   per `tool_use_id`.
//!
//! Rules: canonical content is the block content (what the model received);
//! envelope fields duplicating it, the diff or the call's own arguments are
//! listed under `deduplicated` instead of being copied again. A result with no
//! content and no envelope stays unavailable (`output: None`), distinct from a
//! received empty result. Diffs come only from native `structuredPatch`,
//! Write `create` content, Write before/after content or a NotebookEdit cell's
//! native old/new source; nothing is fabricated from `old_string/new_string`.

use std::collections::{HashMap, HashSet, VecDeque};

use luna_proto::{AgentEvent, ToolCall, ToolDiff};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::normalize::decode_tool_use;
use super::wire::ContentBlock;

/// Envelope schema version for `kind: "native"` (see `luna_doc::native_output`).
const ENVELOPE_VERSION: u64 = 1;
/// Remembered tool calls per run (root + children). Oldest are forgotten first.
const MAX_LEDGER_CALLS: usize = 2_048;
/// Unknown content blocks are retained verbatim up to this size.
const MAX_UNKNOWN_BLOCK_BYTES: usize = 64 * 1024;
/// Arguments already carried by an `Mcp`/`Unknown` call are copied into the
/// envelope (for the detail view) only up to this size.
const MAX_DUPLICATED_INPUT_BYTES: usize = 64 * 1024;
/// Envelope strings shorter than this are never deduplicated by containment.
const MIN_DEDUP_BYTES: usize = 32;

/// MCP servers whose image bytes are never retained (desktop screenshots).
fn is_computer_use_server(tool: &str) -> bool {
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(server, _)| {
            matches!(
                server.to_ascii_lowercase().as_str(),
                "computer-use" | "computer_use" | "computeruse"
            )
        })
}

struct NativeCall {
    name: String,
    input: Value,
    /// Hash of the last delivered result projection (replay dedupe).
    delivered: Option<[u8; 32]>,
}

/// Per-run memory of native tool calls, keyed by owner scope and tool id so a
/// child's colliding id can never replace its parent's call (or vice versa).
#[derive(Default)]
pub(crate) struct ToolLedger {
    calls: HashMap<(String, String), NativeCall>,
    order: VecDeque<(String, String)>,
    /// Successful `StructuredOutput` values seen in this run (canonical JSON).
    structured_outputs: HashSet<String>,
    synthetic_structured: u64,
}

fn scope_key(parent: Option<&str>, id: &str) -> (String, String) {
    (parent.unwrap_or_default().to_owned(), id.to_owned())
}

impl ToolLedger {
    /// Remember a `tool_use` block's native name and complete input.
    pub(crate) fn record(&mut self, parent: Option<&str>, id: &str, name: &str, input: &Value) {
        if id.is_empty() {
            return;
        }
        let key = scope_key(parent, id);
        if !self.calls.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.calls.insert(
            key,
            NativeCall {
                name: name.to_owned(),
                input: input.clone(),
                delivered: None,
            },
        );
        while self.order.len() > MAX_LEDGER_CALLS {
            if let Some(old) = self.order.pop_front() {
                self.calls.remove(&old);
            }
        }
    }

    /// Project every `tool_result` block of one user frame, in block order.
    /// The frame-level `tool_use_result` is attributed only when the frame
    /// carries exactly one result block (it has no id of its own).
    pub(crate) fn results(
        &mut self,
        parent: Option<&str>,
        content: &Value,
        envelope: Option<&Value>,
        metas: Option<&Value>,
    ) -> Vec<AgentEvent> {
        // Same block admission as the frame decoder: a block that does not
        // decode as a `ContentBlock` is skipped, exactly as before.
        let blocks: Vec<(ContentBlock, &Value)> = content
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|raw| {
                        let block = ContentBlock::deserialize(raw).ok()?;
                        (block.kind == "tool_result").then_some((block, raw))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let envelope = envelope.filter(|_| blocks.len() == 1);
        blocks
            .into_iter()
            .filter_map(|(block, raw)| {
                let id = block.tool_use_id.as_str();
                let meta = metas.and_then(Value::as_array).and_then(|metas| {
                    metas
                        .iter()
                        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
                });
                let is_error = block.is_error.unwrap_or(false);
                self.project(parent, id, raw, is_error, envelope, meta)
            })
            .collect()
    }

    fn project(
        &mut self,
        parent: Option<&str>,
        id: &str,
        block: &Value,
        is_error: bool,
        envelope: Option<&Value>,
        meta: Option<&Value>,
    ) -> Option<AgentEvent> {
        let key = scope_key(parent, id);
        let (name, input) = self
            .calls
            .get(&key)
            .map(|call| (call.name.clone(), call.input.clone()))
            .unwrap_or_default();
        let projection = project_result(&name, &input, block, envelope, meta, is_error);
        let event = AgentEvent::ToolResult {
            id: id.to_owned(),
            is_error,
            output: projection.output,
            diff: projection.diff,
        };
        let digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&event).unwrap_or_default()).into();
        if let Some(call) = self.calls.get_mut(&key) {
            if call.delivered == Some(digest) {
                // An identical replay of an already delivered result.
                return None;
            }
            call.delivered = Some(digest);
        }
        if name == "StructuredOutput"
            && !is_error
            && let Some(canonical) = canonical_json(&input)
        {
            self.structured_outputs.insert(canonical);
        }
        Some(event)
    }

    /// A result frame's `structured_output` that no successful
    /// `StructuredOutput` call in this run already delivered becomes its own
    /// part, distinct from the prose `result` (never folded into it).
    pub(crate) fn structured_output(&mut self, value: &Value) -> Vec<AgentEvent> {
        if value.is_null() {
            return Vec::new();
        }
        if canonical_json(value).is_some_and(|c| self.structured_outputs.contains(&c)) {
            return Vec::new();
        }
        self.synthetic_structured += 1;
        let id = format!("structured-output-{}", self.synthetic_structured);
        let envelope = json!({"lunaToolOutput": {
            "kind": "native", "version": ENVELOPE_VERSION, "provider": "claudeCode",
            "tool": "StructuredOutput", "source": "resultFrame", "isError": false,
            "content": [], "empty": false, "structuredContent": value,
        }});
        vec![
            AgentEvent::ToolCall {
                id: id.clone(),
                call: ToolCall::Unknown {
                    name: "StructuredOutput".into(),
                    input: None,
                },
            },
            AgentEvent::ToolResult {
                id,
                is_error: false,
                output: Some(envelope.to_string()),
                diff: None,
            },
        ]
    }
}

fn canonical_json(value: &Value) -> Option<String> {
    // serde_json maps are ordered (BTreeMap) without `preserve_order`, so the
    // serialization is canonical for equal values.
    serde_json::to_string(value).ok()
}

struct Projection {
    output: Option<String>,
    diff: Option<ToolDiff>,
}

fn project_result(
    name: &str,
    input: &Value,
    block: &Value,
    envelope: Option<&Value>,
    meta: Option<&Value>,
    is_error: bool,
) -> Projection {
    let block_content = block.get("content").filter(|v| !v.is_null());
    let envelope = envelope.filter(|v| !v.is_null());
    if block_content.is_none() && envelope.is_none() {
        // Nothing received: unavailable, never an empty result.
        return Projection {
            output: None,
            diff: None,
        };
    }
    let private_images = is_computer_use_server(name);
    let mut deduplicated: Vec<String> = Vec::new();
    let mut omitted = Map::new();

    // MCP calls carry protocol metadata beside the content they returned.
    let (envelope_content, mcp_meta) = split_mcp_envelope(envelope);
    let content_source = match (block_content, envelope_content) {
        (Some(content), Some(_)) => {
            deduplicated.push("content".into());
            Some(content)
        }
        (Some(content), None) => Some(content),
        (None, Some(content)) => Some(content),
        (None, None) => None,
    };
    let content = content_source
        .map(|c| typed_content(c, private_images))
        .unwrap_or_default();
    let text = content
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");

    let mut diff = None;
    let mut result = None;
    if let Some(native) = envelope.filter(|_| mcp_meta.is_none()) {
        let mut native = native.clone();
        diff = native_diff(name, input, &native);
        if let Value::Object(fields) = &mut native {
            prune_native_result(
                name,
                input,
                fields,
                &text,
                content_source,
                diff.as_ref(),
                &mut deduplicated,
                &mut omitted,
            );
        } else if native == Value::String(text.clone())
            || native.as_str().is_some_and(|s| contained(s, &text))
            || content_source.is_some_and(|c| *c == native)
        {
            deduplicated.push("result".into());
            native = Value::Null;
        }
        if private_images {
            scrub_media(&mut native);
        }
        if !native.is_null() && native.as_object().is_none_or(|o| !o.is_empty()) {
            result = Some(native);
        }
    }

    let call = decode_tool_use(name, input);
    let (kept_input, input_note) = if name == "StructuredOutput" && !is_error {
        (None, Some("input:structuredContent".to_owned()))
    } else {
        retained_input(&call, input)
    };
    if let Some(note) = input_note {
        deduplicated.push(note);
    }

    let mut payload = Map::new();
    payload.insert("kind".into(), json!("native"));
    payload.insert("version".into(), json!(ENVELOPE_VERSION));
    payload.insert("provider".into(), json!("claudeCode"));
    payload.insert(
        "tool".into(),
        json!(if name.is_empty() { "unknown" } else { name }),
    );
    payload.insert("isError".into(), json!(is_error));
    let empty = content_source.is_some() && is_empty_text_only(&content);
    payload.insert("empty".into(), json!(empty));
    payload.insert("content".into(), Value::Array(content));
    if let Some(meta) = mcp_meta {
        for (wire, key) in [
            ("structuredContent", "structuredContent"),
            ("_meta", "meta"),
            ("resourceLinks", "resourceLinks"),
        ] {
            if let Some(value) = meta.get(wire).filter(|v| !v.is_null()) {
                payload.insert(key.into(), value.clone());
            }
        }
    }
    if name == "StructuredOutput" && !is_error && input.is_object() {
        // The schema-validated value is the call's own input.
        payload.insert("structuredContent".into(), input.clone());
    }
    if let Some(input) = kept_input {
        payload.insert("input".into(), input);
    }
    if let Some(result) = result {
        payload.insert("result".into(), result);
    }
    if let Some(meta) = meta.and_then(non_execution) {
        payload.insert("nonExecution".into(), meta);
    }
    if !deduplicated.is_empty() {
        payload.insert("deduplicated".into(), json!(deduplicated));
    }
    if !omitted.is_empty() {
        payload.insert("omitted".into(), Value::Object(omitted));
    }
    Projection {
        output: Some(json!({ "lunaToolOutput": Value::Object(payload) }).to_string()),
        diff,
    }
}

fn is_empty_text_only(content: &[Value]) -> bool {
    content.iter().all(|b| {
        b.get("type").and_then(Value::as_str) == Some("text")
            && b.get("text")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
    })
}

/// Replace encoded media anywhere in a private (computer-use) native result
/// with its byte count, the same contract as its content blocks.
fn scrub_media(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for key in ["data", "base64", "blob"] {
                if let Some(Value::String(data)) = fields.get(key) {
                    let bytes = data.len();
                    fields.remove(key);
                    fields.insert(format!("{key}Omitted"), json!({ "bytes": bytes }));
                }
            }
            fields.values_mut().for_each(scrub_media);
        }
        Value::Array(items) => items.iter_mut().for_each(scrub_media),
        _ => {}
    }
}

fn contained(needle: &str, haystack: &str) -> bool {
    needle.len() >= MIN_DEDUP_BYTES && haystack.contains(needle)
}

/// MCP results that carried metadata arrive as `{content, _meta?,
/// structuredContent?, resourceLinks?}`; anything else is a native Output.
fn split_mcp_envelope(envelope: Option<&Value>) -> (Option<&Value>, Option<&Map<String, Value>>) {
    let Some(object) = envelope.and_then(Value::as_object) else {
        return (None, None);
    };
    let is_mcp = object.contains_key("content")
        && ["_meta", "structuredContent", "resourceLinks"]
            .iter()
            .any(|k| object.contains_key(*k))
        && object.keys().all(|k| {
            matches!(
                k.as_str(),
                "content" | "_meta" | "structuredContent" | "resourceLinks"
            )
        });
    if is_mcp {
        (object.get("content").filter(|v| !v.is_null()), Some(object))
    } else {
        (None, None)
    }
}

/// Ordered typed content in the shared viewer's vocabulary (`text`, `image`
/// with `mimeType`, `document`, `resource_link`, `search_result`, else
/// `unsupported`). Encoded media is kept host-side unless it is private.
fn typed_content(content: &Value, private_images: bool) -> Vec<Value> {
    match content {
        Value::String(text) => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({"type": "text", "text": text})]
            }
        }
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| typed_block(block, private_images))
            .collect(),
        other => vec![unsupported_block(other)],
    }
}

fn typed_block(block: &Value, private_images: bool) -> Value {
    let kind = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "text" => {
            let mut out = json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).unwrap_or_default(),
            });
            for key in ["citations", "_meta"] {
                if let Some(value) = block.get(key).filter(|v| !v.is_null()) {
                    out[key] = value.clone();
                }
            }
            out
        }
        "image" | "document" => media_block(kind, block, private_images),
        "search_result" | "resource_link" => bounded_native(block, kind),
        _ => unsupported_block(block),
    }
}

fn media_block(kind: &str, block: &Value, private_images: bool) -> Value {
    let source = block.get("source").cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("type".into(), json!(kind));
    if let Some(mime) = source
        .get("media_type")
        .or_else(|| block.get("mimeType"))
        .and_then(Value::as_str)
    {
        out.insert("mimeType".into(), json!(mime));
    }
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let data = source
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if private_images && kind == "image" {
                out.insert("dataOmitted".into(), json!({ "bytes": data.len() }));
            } else {
                out.insert("data".into(), json!(data));
            }
        }
        Some("url") => {
            if let Some(url) = source.get("url") {
                out.insert("url".into(), url.clone());
            }
        }
        Some("text") => {
            if let Some(data) = source.get("data") {
                out.insert("text".into(), data.clone());
            }
        }
        Some(other) => {
            out.insert("sourceType".into(), json!(other));
        }
        None => {}
    }
    for key in ["title", "context", "citations"] {
        if let Some(value) = block.get(key).filter(|v| !v.is_null()) {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}

fn bounded_native(block: &Value, kind: &str) -> Value {
    let bytes = serde_json::to_vec(block).map_or(0, |b| b.len());
    if bytes <= MAX_UNKNOWN_BLOCK_BYTES {
        block.clone()
    } else {
        json!({"type": kind, "nativeBytes": bytes})
    }
}

/// A content block this version does not understand: visible type plus field
/// names, and the native value retained host-side when bounded.
fn unsupported_block(block: &Value) -> Value {
    let native_type = block
        .get("type")
        .and_then(Value::as_str)
        .map(|t| {
            t.chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect::<String>()
        })
        .unwrap_or_else(|| "unknown".into());
    let fields: Vec<String> = block
        .as_object()
        .map(|o| {
            o.keys()
                .take(32)
                .map(|k| k.chars().filter(|c| !c.is_control()).take(80).collect())
                .collect()
        })
        .unwrap_or_default();
    let bytes = serde_json::to_vec(block).map_or(0, |b| b.len());
    let mut out = json!({
        "type": "unsupported", "nativeType": native_type, "fields": fields,
    });
    if bytes <= MAX_UNKNOWN_BLOCK_BYTES {
        out["native"] = block.clone();
    } else {
        out["nativeBytes"] = json!(bytes);
    }
    out
}

fn non_execution(meta: &Value) -> Option<Value> {
    let mut out = Map::new();
    for (wire, key) in [
        ("non_execution_kind", "kind"),
        ("user_feedback", "userFeedback"),
        ("remedy", "remedy"),
    ] {
        if let Some(value) = meta.get(wire).filter(|v| !v.is_null()) {
            out.insert(key.into(), value.clone());
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

/// Diffs only from supplied native evidence.
fn native_diff(name: &str, input: &Value, native: &Value) -> Option<ToolDiff> {
    let fields = native.as_object()?;
    if fields.get("staged").and_then(Value::as_bool) == Some(true) {
        // Held for review: the file is unchanged.
        return None;
    }
    let path = fields
        .get("filePath")
        .and_then(Value::as_str)
        .or_else(|| input.get("file_path").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    match name {
        "Edit" | "MultiEdit" | "Write" => {
            let content = fields.get("content").and_then(Value::as_str);
            let original = fields.get("originalFile").and_then(Value::as_str);
            if name == "Write"
                && fields.get("type").and_then(Value::as_str) == Some("create")
                && let Some(content) = content
            {
                return Some(ToolDiff {
                    path,
                    old_text: None,
                    new_text: content.to_owned(),
                    unified_diff: None,
                });
            }
            if let Some(patch) = structured_patch(fields.get("structuredPatch")) {
                return Some(ToolDiff {
                    path,
                    old_text: None,
                    new_text: String::new(),
                    unified_diff: Some(patch),
                });
            }
            if name == "Write"
                && let (Some(original), Some(content)) = (original, content)
                && original != content
            {
                return Some(ToolDiff {
                    path,
                    old_text: Some(original.to_owned()),
                    new_text: content.to_owned(),
                    unified_diff: None,
                });
            }
            None
        }
        "NotebookEdit" => {
            if fields.get("error").is_some_and(|e| !e.is_null()) {
                return None;
            }
            let new_source = fields.get("new_source").and_then(Value::as_str)?;
            let old_source = fields.get("old_source").and_then(Value::as_str);
            let mode = fields
                .get("edit_mode")
                .and_then(Value::as_str)
                .unwrap_or("replace");
            let notebook = fields
                .get("notebook_path")
                .and_then(Value::as_str)
                .or_else(|| input.get("notebook_path").and_then(Value::as_str))
                .unwrap_or_default();
            let cell = fields
                .get("cell_id")
                .and_then(Value::as_str)
                .unwrap_or("new");
            let (old_text, new_text) = match mode {
                "delete" => (Some(old_source?.to_owned()), String::new()),
                "insert" => (None, new_source.to_owned()),
                _ => (Some(old_source?.to_owned()), new_source.to_owned()),
            };
            Some(ToolDiff {
                path: format!("{notebook}#cell-{cell}"),
                old_text,
                new_text,
                unified_diff: None,
            })
        }
        _ => None,
    }
}

/// `structuredPatch` hunks → unified hunks. Malformed hunks yield no diff.
fn structured_patch(patch: Option<&Value>) -> Option<String> {
    let hunks = patch?.as_array()?;
    if hunks.is_empty() {
        return None;
    }
    let mut out = String::new();
    for hunk in hunks {
        let number = |key: &str| hunk.get(key).and_then(Value::as_u64);
        let (Some(old_start), Some(old_lines), Some(new_start), Some(new_lines)) = (
            number("oldStart"),
            number("oldLines"),
            number("newStart"),
            number("newLines"),
        ) else {
            return None;
        };
        let lines = hunk.get("lines")?.as_array()?;
        out.push_str(&format!(
            "@@ -{old_start},{old_lines} +{new_start},{new_lines} @@\n"
        ));
        for line in lines {
            let line = line.as_str()?;
            if !matches!(line.chars().next(), Some(' ' | '-' | '+' | '\\')) {
                return None;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

/// Drop envelope fields that duplicate the canonical content, the diff or the
/// call's arguments; record what was removed and why.
fn prune_native_result(
    name: &str,
    input: &Value,
    fields: &mut Map<String, Value>,
    text: &str,
    content: Option<&Value>,
    diff: Option<&ToolDiff>,
    deduplicated: &mut Vec<String>,
    omitted: &mut Map<String, Value>,
) {
    fn drop_field(
        fields: &mut Map<String, Value>,
        deduplicated: &mut Vec<String>,
        key: &str,
        why: &str,
    ) {
        if fields.remove(key).is_some() {
            deduplicated.push(format!("{key}:{why}"));
        }
    }
    if let Some(diff) = diff {
        drop_field(fields, deduplicated, "structuredPatch", "diff");
        if name == "Write" && diff.unified_diff.is_none() {
            // create content / full before-after pair live in the diff.
            drop_field(fields, deduplicated, "content", "diff");
            drop_field(fields, deduplicated, "originalFile", "diff");
        }
    }
    if let Some(original) = fields.remove("originalFile")
        && let Some(original) = original.as_str()
    {
        // The pre-image can be a whole file; the native patch is its evidence.
        omitted.insert("originalFile".into(), json!({ "bytes": original.len() }));
    }
    if name == "Read"
        && let Some(file) = fields.get_mut("file").and_then(Value::as_object_mut)
    {
        if !text.is_empty()
            && let Some(content) = file.remove("content")
        {
            let bytes = content.as_str().map_or(0, str::len);
            omitted.insert(
                "file.content".into(),
                json!({ "bytes": bytes, "canonical": "content" }),
            );
        }
        if file.remove("base64").is_some() {
            deduplicated.push("file.base64:content".into());
        }
    }
    // A native report repeating the block content (Agent `content`).
    if content.is_some() && fields.get("content") == content {
        drop_field(fields, deduplicated, "content", "content");
    }
    // Arguments echoed back by the tool (Edit oldString/newString, Write content).
    for (key, arg) in [
        ("oldString", "old_string"),
        ("newString", "new_string"),
        ("content", "content"),
    ] {
        if fields.get(key).is_some_and(|v| Some(v) == input.get(arg)) {
            drop_field(fields, deduplicated, key, "input");
        }
    }
    // Generic: long strings already present verbatim in the canonical text.
    let echoed: Vec<String> = fields
        .iter()
        .filter(|(_, v)| v.as_str().is_some_and(|s| contained(s, text)))
        .map(|(k, _)| k.clone())
        .collect();
    for key in echoed {
        drop_field(fields, deduplicated, &key, "content");
    }
}

/// Complete native arguments for the detail view. Typed calls keep only one
/// or two fields, so the envelope carries everything except large strings the
/// typed call already retains verbatim; `Mcp`/`Unknown` calls keep their full
/// input on the call, so a copy is added only when it is small.
fn retained_input(call: &ToolCall, input: &Value) -> (Option<Value>, Option<String>) {
    let Some(object) = input.as_object() else {
        return (None, None);
    };
    if object.is_empty() {
        return (None, None);
    }
    match call {
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => {
            let bytes = serde_json::to_vec(input).map_or(0, |b| b.len());
            if bytes <= MAX_DUPLICATED_INPUT_BYTES {
                (Some(input.clone()), None)
            } else {
                (None, Some("input:call".into()))
            }
        }
        _ => {
            let in_call: &[&str] = match call {
                ToolCall::WriteFile { .. } => &["content"],
                ToolCall::EditFile { .. } => &["old_string", "new_string"],
                _ => &[],
            };
            let mut kept = object.clone();
            let mut moved = Vec::new();
            for key in in_call {
                if kept
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.len() >= MIN_DEDUP_BYTES)
                {
                    kept.remove(*key);
                    moved.push(*key);
                }
            }
            let note = (!moved.is_empty()).then(|| format!("input.{}:call", moved.join(",")));
            (Some(Value::Object(kept)), note)
        }
    }
}
