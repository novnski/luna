//! OpenCode tool-result projection (OC-06): every terminal tool frame
//! becomes the shared `lunaToolOutput` envelope with `kind: "native"` (the
//! Claude adapter's contract, `docs/reference/tool-output.md`), so the host
//! journal, lazy fetch, doc preview and output viewer treat OpenCode exactly
//! like Claude Code. Nothing is capped here: the complete received result
//! reaches the host journal, and the doc keeps its own preview limits.
//!
//! Source of the shapes (2.0.22, `packages/schema/src/session-event.ts`):
//! - `session.tool.success {content: NonEmpty<text|file>, metadata?,
//!   executed, resultState?}`; `file` is `{uri, mime, name?}` (a `data:`
//!   URI for images/PDFs read by `read`, MCP media and Code Mode files).
//! - `session.tool.failed {error: {type, message}, content?, metadata?,
//!   executed, resultState?}`: the error plus the final snapshot of partial
//!   progress (`metadata` merges the last `session.tool.progress`).
//! - `metadata.files: FileDiff[] {file, patch, additions, deletions,
//!   status}` on `edit` and `patch` success. `write` reports none; its
//!   "Created file" receipt plus the call's own content is the native
//!   evidence of a new file (an overwrite has no before-image: no diff).
//! - shell metadata `{status?, truncated, exit?, signal?, timeout?,
//!   shellID?}`; natively truncated non-shell output carries
//!   `{truncated: true, outputPath}` (the managed file in OpenCode's data
//!   directory, recorded here, never read).
//!
//! Rules (mirroring the Claude projection): content keeps native order;
//! encoded media stays host-side with its byte count; the error message is
//! the first text block of a failed result (what the model saw), any partial
//! content follows and the envelope says `partial`; a received empty result
//! is `empty: true`, distinct from an unavailable one (no terminal frame →
//! no `ToolResult` at all). Unknown content items keep their type and field
//! names (bounded). Fields that repeat a diff or the call's arguments are
//! listed under `deduplicated`; oversized values under `omitted`.

use serde_json::Map;

use super::*;

/// Envelope schema version for `kind: "native"` (see `luna_doc::native_output`).
const ENVELOPE_VERSION: u64 = 1;
/// Unknown content items are retained verbatim up to this size.
const MAX_UNKNOWN_ITEM_BYTES: usize = 64 * 1024;
/// Encoded media retained per item (base64 characters); larger is omitted.
const MAX_MEDIA_CHARS: usize = 8 * 1024 * 1024;
/// Arguments an `Mcp`/`Unknown` call already carries are copied into the
/// envelope only up to this size.
const MAX_DUPLICATED_INPUT_BYTES: usize = 64 * 1024;
/// Typed calls' complete arguments are kept up to this size.
const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Metadata / provider state retained whole up to this size; beyond it,
/// each oversized field is replaced by its byte count.
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_METADATA_FIELD_BYTES: usize = 4 * 1024;
/// Strings shorter than this are never moved out of the arguments.
const MIN_DEDUP_BYTES: usize = 32;

/// One terminal tool frame, as the normalizer hands it over (`state`), with
/// what the feed remembered from the call.
pub(super) struct Terminal<'a> {
    pub(super) tool: &'a str,
    pub(super) input: &'a Value,
    pub(super) call: &'a ToolCall,
    pub(super) state: &'a Value,
    pub(super) live: Option<&'a LiveOutput>,
}

/// One native per-file change and the diff LUNA's diff surfaces render.
pub(super) struct FileChange {
    pub(super) path: String,
    pub(super) status: String,
    pub(super) diff: Option<ToolDiff>,
}

pub(super) struct Projection {
    pub(super) is_error: bool,
    pub(super) output: Option<String>,
    /// Native per-file changes, in native order (`edit`/`write`: at most
    /// one; `patch`: one per file operation).
    pub(super) files: Vec<FileChange>,
}

impl FileChange {
    /// The chip for a file the call's arguments did not announce.
    fn call(&self) -> ToolCall {
        match self.status.as_str() {
            "added" => ToolCall::WriteFile {
                path: self.path.clone(),
                content: None,
            },
            "deleted" => ToolCall::ApplyPatch {
                path: Some(self.path.clone()),
            },
            _ => ToolCall::EditFile {
                path: self.path.clone(),
                old_string: None,
                new_string: None,
            },
        }
    }
}

/// The result events of a settled call: the envelope on its first chip and
/// one result per file chip, each with its native diff (per-file chips are
/// matched in native order, the order the patch announced its files).
pub(super) fn tool_result_events(
    call_id: &str,
    entry: &PartState,
    state: &Value,
) -> Vec<AgentEvent> {
    let fallback = ToolCall::Unknown {
        name: entry.tool.clone(),
        input: None,
    };
    let call = entry.call.as_ref().unwrap_or(&fallback);
    let projection = project_terminal(Terminal {
        tool: &entry.tool,
        input: &entry.input,
        call,
        state,
        live: entry.live.as_ref(),
    });
    let chips: Vec<String> = if entry.chips.is_empty() {
        vec![call_id.to_owned()]
    } else {
        entry.chips.clone()
    };
    let count = chips.len().max(projection.files.len());
    let mut events = Vec::new();
    for index in 0..count {
        let file = projection.files.get(index);
        let id = match chips.get(index) {
            Some(id) => id.clone(),
            None => {
                let id = chip_id(call_id, index);
                if let Some(file) = file {
                    events.push(AgentEvent::ToolCall {
                        id: id.clone(),
                        call: file.call(),
                    });
                }
                id
            }
        };
        events.push(AgentEvent::ToolResult {
            id,
            is_error: projection.is_error,
            output: if index == 0 {
                projection.output.clone()
            } else {
                None
            },
            diff: file.and_then(|f| f.diff.clone()),
        });
    }
    events
}

#[derive(Default)]
struct Notes {
    deduplicated: Vec<String>,
    omitted: Map<String, Value>,
}

/// Project one terminal frame into the shared envelope and its diffs.
pub(super) fn project_terminal(terminal: Terminal<'_>) -> Projection {
    let Terminal {
        tool,
        input,
        call,
        state,
        live,
    } = terminal;
    let is_error = state.get("status").and_then(Value::as_str) == Some("error");
    let mut notes = Notes::default();

    // Native content: the v2 array, or a 1.x-shaped text `output` for a
    // part that never carried one (fixtures, imported records).
    let native_content: Vec<Value> = match state.get("content").and_then(Value::as_array) {
        Some(items) => items.clone(),
        None => state
            .get("output")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && !is_error)
            .map(|text| vec![json!({"type": "text", "text": text})])
            .unwrap_or_default(),
    };
    let mut content: Vec<Value> = Vec::new();
    let mut error = None;
    if is_error {
        let message = state
            .get("error")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .unwrap_or("The tool failed")
            .to_owned();
        let mut detail = Map::new();
        if let Some(kind) = state
            .get("errorType")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
        {
            detail.insert("type".into(), json!(kind));
        }
        detail.insert("message".into(), json!(message));
        error = Some(Value::Object(detail));
        content.push(json!({"type": "text", "text": message}));
    }
    content.extend(native_content.iter().map(typed_item));

    let metadata = state
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let files = native_files(tool, input, state, &metadata, is_error);
    let metadata = project_metadata(metadata, &files, &mut notes);

    let (kept_input, input_note) = retained_input(call, input);
    if let Some(note) = input_note {
        notes.deduplicated.push(note);
    }

    let mut payload = Map::new();
    payload.insert("kind".into(), json!("native"));
    payload.insert("version".into(), json!(ENVELOPE_VERSION));
    payload.insert("provider".into(), json!("opencode"));
    payload.insert(
        "tool".into(),
        json!(if tool.is_empty() { "unknown" } else { tool }),
    );
    payload.insert("isError".into(), json!(is_error));
    let empty = !is_error && is_empty_text_only(&content);
    payload.insert("empty".into(), json!(empty));
    // A failed call that still produced output (a final progress snapshot or
    // live shell output) keeps it, labelled partial.
    if is_error && (!native_content.is_empty() || live.is_some_and(LiveOutput::has_bytes)) {
        payload.insert("partial".into(), json!(true));
    }
    payload.insert("content".into(), Value::Array(content));
    if let Some(error) = error {
        payload.insert("error".into(), error);
    }
    if !metadata.is_empty() {
        payload.insert("metadata".into(), Value::Object(metadata));
    }
    if let Some(live) = live {
        let mut live = live.envelope();
        // The native tail preview the complete capture replaces in the
        // detail view: the first native text block.
        let offset = usize::from(is_error);
        if let Some(index) = native_content
            .iter()
            .position(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        {
            live["replacesBlock"] = json!(offset + index);
        }
        payload.insert("liveOutput".into(), live);
    }
    if let Some(input) = kept_input {
        payload.insert("input".into(), input);
    }
    if let Some(result_state) = state.get("resultState").filter(|v| !v.is_null()) {
        let bytes = serde_json::to_vec(result_state).map_or(0, |b| b.len());
        if bytes <= MAX_METADATA_FIELD_BYTES {
            payload.insert("resultState".into(), result_state.clone());
        } else {
            notes
                .omitted
                .insert("resultState".into(), json!({ "bytes": bytes }));
        }
    }
    if state.get("executed").and_then(Value::as_bool) == Some(true) {
        payload.insert("providerExecuted".into(), json!(true));
    }
    if !notes.deduplicated.is_empty() {
        payload.insert("deduplicated".into(), json!(notes.deduplicated));
    }
    if !notes.omitted.is_empty() {
        payload.insert("omitted".into(), Value::Object(notes.omitted));
    }
    Projection {
        is_error,
        output: Some(json!({ "lunaToolOutput": Value::Object(payload) }).to_string()),
        files,
    }
}

fn is_empty_text_only(content: &[Value]) -> bool {
    content.iter().all(|item| {
        item.get("type").and_then(Value::as_str) == Some("text")
            && item
                .get("text")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
    })
}

/// One native content item in the shared viewer's vocabulary: `text`;
/// `image`/`document` with `mimeType`, base64 `data` and decoded `bytes`
/// for a `data:` file; `resource_link` for any other file URI; else
/// `unsupported` with its type and field names.
fn typed_item(item: &Value) -> Value {
    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "text" => json!({
            "type": "text",
            "text": item.get("text").and_then(Value::as_str).unwrap_or_default(),
        }),
        "file" => file_item(item),
        _ => unsupported_item(item),
    }
}

fn file_item(item: &Value) -> Value {
    let uri = item.get("uri").and_then(Value::as_str).unwrap_or_default();
    let mime = item
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let mut out = Map::new();
    match uri
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
    {
        Some((_, data)) => {
            let kind = if mime.starts_with("image/") {
                "image"
            } else {
                "document"
            };
            out.insert("type".into(), json!(kind));
            out.insert("mimeType".into(), json!(mime));
            out.insert("bytes".into(), json!(base64_decoded_len(data)));
            if data.len() <= MAX_MEDIA_CHARS {
                out.insert("data".into(), json!(data));
            } else {
                out.insert("dataOmitted".into(), json!({ "bytes": data.len() }));
            }
        }
        None => {
            out.insert("type".into(), json!("resource_link"));
            out.insert("uri".into(), json!(uri));
            out.insert("mimeType".into(), json!(mime));
        }
    }
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        out.insert("name".into(), json!(name));
    } else if out.get("type") == Some(&json!("resource_link")) {
        out.insert("name".into(), json!(uri.rsplit('/').next().unwrap_or(uri)));
    }
    Value::Object(out)
}

/// Decoded size of a base64 payload (padding-aware, whitespace ignored).
fn base64_decoded_len(data: &str) -> u64 {
    let significant = data
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .count() as u64;
    significant * 3 / 4
}

/// A content item this version does not understand: visible type plus field
/// names, the native value retained host-side when bounded.
fn unsupported_item(item: &Value) -> Value {
    let native_type = item
        .get("type")
        .and_then(Value::as_str)
        .map(diagnostic_label)
        .unwrap_or_else(|| "unknown".into());
    let fields: Vec<String> = item
        .as_object()
        .map(|o| o.keys().take(32).map(|k| diagnostic_label(k)).collect())
        .unwrap_or_default();
    let bytes = serde_json::to_vec(item).map_or(0, |b| b.len());
    let mut out = json!({"type": "unsupported", "nativeType": native_type, "fields": fields});
    if bytes <= MAX_UNKNOWN_ITEM_BYTES {
        out["native"] = item.clone();
    } else {
        out["nativeBytes"] = json!(bytes);
    }
    out
}

/// Native per-file changes and their diffs. Diffs come only from native
/// evidence: `metadata.files` (`edit`, `patch`; also the partial snapshot a
/// failed call may carry) or a `write` that reports creating its file.
fn native_files(
    tool: &str,
    input: &Value,
    state: &Value,
    metadata: &Map<String, Value>,
    is_error: bool,
) -> Vec<FileChange> {
    if let Some(files) = metadata.get("files").and_then(Value::as_array) {
        let changes: Vec<FileChange> = files
            .iter()
            .filter_map(|file| {
                let path = file.get("file").and_then(Value::as_str)?.to_owned();
                let status = file
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("modified")
                    .to_owned();
                let diff = (!is_error)
                    .then(|| file.get("patch").and_then(Value::as_str))
                    .flatten()
                    .and_then(unified_hunks)
                    .map(|hunks| ToolDiff {
                        path: path.clone(),
                        old_text: None,
                        new_text: String::new(),
                        unified_diff: Some(hunks),
                    });
                Some(FileChange { path, status, diff })
            })
            .collect();
        if !changes.is_empty() {
            return changes;
        }
    }
    if tool == "write" && !is_error {
        let created = state
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .any(|text| text.starts_with("Created file successfully"));
        if created
            && let (Some(path), Some(content)) = (
                input.get("path").and_then(Value::as_str),
                input.get("content").and_then(Value::as_str),
            )
        {
            return vec![FileChange {
                path: path.to_owned(),
                status: "added".into(),
                diff: Some(ToolDiff {
                    path: path.to_owned(),
                    old_text: None,
                    new_text: content.to_owned(),
                    unified_diff: None,
                }),
            }];
        }
    }
    Vec::new()
}

/// A `createTwoFilesPatch` result (`Index:`/`===`/`---`/`+++` headers, then
/// hunks) reduced to its hunks, the shape Claude's diffs carry. A patch with
/// no hunk (an unchanged file) yields no diff.
fn unified_hunks(patch: &str) -> Option<String> {
    let start = if patch.starts_with("@@") {
        0
    } else {
        patch.find("\n@@").map(|at| at + 1)?
    };
    let hunks = &patch[start..];
    (!hunks.trim().is_empty()).then(|| hunks.to_owned())
}

/// Metadata kept for the detail view: once the `files` patches became diffs,
/// only their per-file summary stays here; a failed call keeps its native
/// patches (they were not applied, so they are not shown as diffs). Oversized
/// fields keep only their byte count.
fn project_metadata(
    mut metadata: Map<String, Value>,
    files: &[FileChange],
    notes: &mut Notes,
) -> Map<String, Value> {
    if files.iter().any(|f| f.diff.is_some())
        && let Some(Value::Array(native)) = metadata.get_mut("files")
    {
        for file in native.iter_mut() {
            if let Some(fields) = file.as_object_mut() {
                fields.remove("patch");
            }
        }
        notes.deduplicated.push("metadata.files.patch:diff".into());
    }
    let bytes = serde_json::to_vec(&metadata).map_or(0, |b| b.len());
    if bytes > MAX_METADATA_BYTES {
        let oversized: Vec<String> = metadata
            .iter()
            .filter(|(_, v)| {
                serde_json::to_vec(v).map_or(0, |b| b.len()) > MAX_METADATA_FIELD_BYTES
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in oversized {
            if let Some(value) = metadata.remove(&key) {
                let bytes = serde_json::to_vec(&value).map_or(0, |b| b.len());
                notes
                    .omitted
                    .insert(format!("metadata.{key}"), json!({ "bytes": bytes }));
            }
        }
    }
    metadata
}

/// Complete native arguments for the detail view. Typed calls keep every
/// field except large strings the typed call already retains verbatim;
/// `Mcp`/`Unknown` calls keep their full input on the call, so a copy is
/// added only when it is small.
fn retained_input(call: &ToolCall, input: &Value) -> (Option<Value>, Option<String>) {
    let Some(object) = input.as_object() else {
        return (None, None);
    };
    if object.is_empty() {
        return (None, None);
    }
    let bytes = serde_json::to_vec(input).map_or(0, |b| b.len());
    match call {
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => {
            if bytes <= MAX_DUPLICATED_INPUT_BYTES {
                (Some(input.clone()), None)
            } else {
                (None, Some("input:call".into()))
            }
        }
        _ => {
            let in_call: &[&str] = match call {
                ToolCall::WriteFile { .. } => &["content"],
                ToolCall::EditFile { .. } => &["oldString", "newString"],
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
            let kept = Value::Object(kept);
            if serde_json::to_vec(&kept).map_or(0, |b| b.len()) > MAX_INPUT_BYTES {
                return (None, Some(format!("input:omitted({bytes} bytes)")));
            }
            let note = (!moved.is_empty()).then(|| format!("input.{}:call", moved.join(",")));
            (Some(kept), note)
        }
    }
}
