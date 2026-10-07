//! Tool vocabulary: native tool names onto typed `ToolCall`s, output cap.

use super::*;

/// Cap for tool outputs entering the event stream (journal keeps the rest).
pub(super) const OUTPUT_CAP: usize = 4096;

pub(super) fn cap_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Type an opencode-native tool invocation.
pub(super) fn oc_tool_call(name: &str, input: &Value) -> ToolCall {
    let s = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| input.get(*k))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    match name {
        "bash" => ToolCall::Exec {
            command: s(&["command"]).unwrap_or_default(),
        },
        "read" => ToolCall::ReadFile {
            path: s(&["filePath", "file_path", "path"]).unwrap_or_default(),
        },
        "write" => ToolCall::WriteFile {
            path: s(&["filePath", "file_path", "path"]).unwrap_or_default(),
            content: s(&["content"]),
        },
        "edit" => ToolCall::EditFile {
            path: s(&["filePath", "file_path", "path"]).unwrap_or_default(),
            old_string: s(&["oldString", "old_string"]),
            new_string: s(&["newString", "new_string"]),
        },
        "patch" => ToolCall::ApplyPatch {
            path: s(&["filePath", "file_path", "path"]),
        },
        "grep" => ToolCall::Search {
            pattern: s(&["pattern"]).unwrap_or_default(),
            path: s(&["path", "include"]),
        },
        "glob" => ToolCall::Glob {
            pattern: s(&["pattern"]).unwrap_or_default(),
        },
        "webfetch" => ToolCall::WebFetch {
            url: s(&["url"]).unwrap_or_default(),
            prompt: None,
        },
        "websearch" => ToolCall::WebSearch {
            query: s(&["query"]).unwrap_or_default(),
        },
        "todowrite" => ToolCall::Todo {
            items: input
                .get("todos")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|t| TodoItem {
                    text: t
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    done: t.get("status").and_then(Value::as_str) == Some("completed"),
                })
                .collect(),
        },
        // The genus-gated spawn naming every driver shares (v2 `subagent`,
        // v1 `task`).
        "subagent" | "task" => ToolCall::Unknown {
            name: s(&["description"])
                .map(|d| format!("Agent: {d}"))
                .unwrap_or_else(|| "Agent".into()),
            input: (!input.is_null()).then(|| input.clone()),
        },
        _ => ToolCall::Unknown {
            name: name.to_owned(),
            input: (!input.is_null()).then(|| input.clone()),
        },
    }
}
