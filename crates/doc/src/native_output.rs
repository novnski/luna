//! `lunaToolOutput` kind `native`: a provider-native tool result with ordered
//! typed content (`text`, `image`, `document`, `resource_link`,
//! `search_result`, `unsupported`), complete arguments (`input`), the native
//! result object (`result`), MCP `structuredContent`/`meta`/`resourceLinks`,
//! non-execution evidence and dedupe/omission notes. Produced by the Claude
//! adapter; retained only in the owning host's journal.
//!
//! Synced documents and live previews receive [`native_summary`] (type/count
//! metadata) or, for typed built-in tools, a bounded summary of the text
//! blocks ([`native_text`]). [`native_display`] renders the lazily fetched
//! full envelope for the detail view; encoded media is described, never
//! dumped, and Copy keeps the complete envelope.

use serde_json::Value;

const ENVELOPE_PREFIX: &str = "{\"lunaToolOutput\":";

/// The `native` payload of a journaled tool output, if it is one.
pub fn native_payload(text: &str) -> Option<Value> {
    // Cheap guard: plain command output is never parsed as JSON here.
    if !text.starts_with(ENVELOPE_PREFIX) {
        return None;
    }
    let mut value: Value = serde_json::from_str(text).ok()?;
    let payload = value.get_mut("lunaToolOutput").map(Value::take)?;
    (payload.get("kind").and_then(Value::as_str) == Some("native")).then_some(payload)
}

fn blocks(payload: &Value) -> &[Value] {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn block_type(block: &Value) -> &str {
    block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// The text blocks, in order, joined by newlines (`None` if not native).
pub fn native_text(text: &str) -> Option<String> {
    let payload = native_payload(text)?;
    Some(text_of(&payload))
}

fn text_of(payload: &Value) -> String {
    blocks(payload)
        .iter()
        .filter(|b| block_type(b) == "text")
        .map(|b| field(b, "text"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Type/count metadata only: safe for synced documents and live previews.
pub fn native_summary(payload: &Value, is_error: bool) -> String {
    let status = match payload
        .pointer("/nonExecution/kind")
        .and_then(Value::as_str)
    {
        Some(kind) => format!("Not run ({kind})"),
        None if is_error || payload.get("isError").and_then(Value::as_bool) == Some(true) => {
            "Failed".into()
        }
        None => "Completed".into(),
    };
    let mut labels = Vec::new();
    for (label, plural, kind) in [
        ("text", "text", "text"),
        ("image", "images", "image"),
        ("document", "documents", "document"),
        ("resource link", "resource links", "resource_link"),
        ("search result", "search results", "search_result"),
        ("unsupported block", "unsupported blocks", "unsupported"),
    ] {
        let count = blocks(payload)
            .iter()
            .filter(|b| block_type(b) == kind)
            .count();
        if count > 0 {
            labels.push(format!(
                "{count} {}",
                if count == 1 { label } else { plural }
            ));
        }
    }
    if let Some(links) = payload
        .get("resourceLinks")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
    {
        let count = links.len();
        labels.push(format!(
            "{count} {}",
            if count == 1 {
                "resource link"
            } else {
                "resource links"
            }
        ));
    }
    if payload
        .get("structuredContent")
        .is_some_and(|v| !v.is_null())
    {
        labels.push("structured content".into());
    }
    if payload.get("empty").and_then(Value::as_bool) == Some(true)
        && payload.get("structuredContent").is_none_or(Value::is_null)
    {
        labels.push("empty result".into());
    }
    if labels.is_empty() {
        status
    } else {
        format!("{status} · {}", labels.join(" · "))
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Full, readable rendering of a fetched native envelope (detail view).
pub fn native_display(text: &str) -> Option<String> {
    let payload = native_payload(text)?;
    // Keep the SDK/model preview in the receipt while preferring separately
    // collected bytes in the detail view. Failure evidence stays visible.
    let saved = (payload["tool"] == "Bash"
        && payload["savedOutput"]["state"] == "loaded"
        && payload["savedOutput"]["source"] == "claude-saved-bash-output")
        .then(|| payload["savedOutput"]["text"].as_str())
        .flatten()
        .filter(|text| payload["savedOutput"]["byteLength"].as_u64() == Some(text.len() as u64));
    let content = blocks(&payload);
    let is_error = payload.get("isError").and_then(Value::as_bool) == Some(true);
    let only_text = content.iter().all(|b| block_type(b) == "text");
    let mut sections = Vec::new();
    if payload["savedOutput"]["state"] == "unavailable" {
        sections.push(format!(
            "Saved Bash output not loaded: {}",
            field(&payload["savedOutput"], "reason")
        ));
    }
    if !only_text
        || is_error
        || payload.get("nonExecution").is_some()
        || payload.get("empty").and_then(Value::as_bool) == Some(true)
        || payload
            .get("structuredContent")
            .is_some_and(|v| !v.is_null())
    {
        sections.push(native_summary(&payload, is_error));
    }
    if let Some(non_execution) = payload.get("nonExecution") {
        let mut line = format!("Not executed: {}", field(non_execution, "kind"));
        let feedback = field(non_execution, "userFeedback");
        if !feedback.is_empty() {
            line.push_str(&format!(" — {feedback}"));
        }
        sections.push(line);
    }
    if let Some(saved) = saved {
        sections.push(format!(
            "Full saved Bash output ({} bytes)\n{}",
            saved.len(),
            saved
        ));
    }
    for block in content {
        let media = |block: &Value| {
            if block.get("dataOmitted").is_some() {
                "media not retained".to_owned()
            } else if let Some(data) = block.get("data").and_then(Value::as_str) {
                format!("{} base64 chars retained in full result", data.len())
            } else if let Some(url) = block.get("url").and_then(Value::as_str) {
                format!("source {url}")
            } else {
                "source retained in full result".to_owned()
            }
        };
        match block_type(block) {
            "text" if saved.is_some() => {}
            "text" => sections.push(field(block, "text").to_owned()),
            "image" => sections.push(format!(
                "Image ({}) · {}",
                field(block, "mimeType"),
                media(block)
            )),
            "document" => {
                let title = field(block, "title");
                let mut line =
                    format!("Document ({}) · {}", field(block, "mimeType"), media(block));
                if !title.is_empty() {
                    line = format!("{line}\n{title}");
                }
                sections.push(line);
            }
            "resource_link" => sections.push(format!(
                "Resource link: {}\n{}",
                field(block, "name"),
                field(block, "uri")
            )),
            "search_result" => sections.push(format!(
                "Search result: {}\n{}",
                field(block, "title"),
                field(block, "source")
            )),
            "unsupported" => sections.push(format!(
                "Unsupported content block `{}` · retained in full result",
                field(block, "nativeType")
            )),
            other => sections.push(format!("Content block `{other}` · retained in full result")),
        }
    }
    if let Some(links) = payload.get("resourceLinks").and_then(Value::as_array) {
        for link in links {
            let mut line = format!(
                "Resource link: {}\n{}",
                field(link, "name"),
                field(link, "uri")
            );
            let mime = field(link, "mimeType");
            if !mime.is_empty() {
                line.push_str(&format!(" ({mime})"));
            }
            sections.push(line);
        }
    }
    if let Some(structured) = payload.get("structuredContent").filter(|v| !v.is_null()) {
        sections.push(format!("Structured content\n{}", pretty(structured)));
    }
    if let Some(input) = payload.get("input").filter(|v| !v.is_null()) {
        sections.push(format!("Arguments\n{}", pretty(input)));
    }
    if let Some(result) = payload.get("result").filter(|v| !v.is_null()) {
        let mut result = result.clone();
        if saved.is_some()
            && let Some(fields) = result.as_object_mut()
        {
            // The retained receipt still has stdout; suppress only its
            // duplicated partial preview in the display, never stderr/flags.
            fields.remove("stdout");
        }
        sections.push(format!("Native result\n{}", pretty(&result)));
    }
    if let Some(meta) = payload.get("meta").filter(|v| !v.is_null()) {
        sections.push(format!("Protocol metadata\n{}", pretty(meta)));
    }
    if let Some(omitted) = payload.get("omitted").and_then(Value::as_object) {
        let notes: Vec<String> = omitted
            .iter()
            .map(|(name, info)| {
                let bytes = info.get("bytes").and_then(Value::as_u64).unwrap_or(0);
                format!("{name} ({bytes} bytes)")
            })
            .collect();
        if !notes.is_empty() {
            sections.push(format!("Not retained: {}", notes.join(", ")));
        }
    }
    if sections.is_empty() {
        sections.push(native_summary(&payload, is_error));
    }
    Some(sections.join("\n\n"))
}
