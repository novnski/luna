//! Tool vocabulary: OpenCode 2 tool names onto typed `ToolCall`s (OC-06).
//!
//! The 2.0.22 built-ins (`packages/core/src/tool/plugin/*.ts`) and how each
//! reaches LUNA's shared tool chips:
//! - `shell {command, workdir?, timeout?, background?}` → `Exec{command}`;
//!   the other arguments stay in the result envelope's `input`.
//! - `read {path, offset?, limit?}` → `ReadFile`; `write {path, content}` →
//!   `WriteFile`; `edit {path, oldString, newString, replaceAll?}` →
//!   `EditFile` (v2 renamed `filePath` to `path`).
//! - `patch {patchText}` → one chip per file operation, typed like Codex's
//!   file changes (add = write, update = edit, delete = patch), keyed
//!   `<call>`, `<call>#1`, `<call>#2`… in patch order so the completion's
//!   per-file diffs land on the same chips.
//! - `glob {pattern, path?}` → `Glob`; `grep {pattern, path?, include?}` →
//!   `Search{pattern, path}` (`include` is a file filter, never the path).
//! - `webfetch {url, format}` → `WebFetch`; `websearch {query}` → `WebSearch`.
//! - `subagent {agent, description, prompt, model?, background?}` → the
//!   genus-gated spawn name `Agent: <description>` every driver shares (the
//!   child binding is the subagent package's).
//! - `skill`, `question`, `execute` (Code Mode) and the `opencode_*`
//!   namespace → generic cards with their input preserved, like Claude's
//!   `Skill`/`AskUserQuestion`; `opencode_read_mcp_resource` (and a
//!   server-scoped `opencode_list_mcp_resources`) → `Mcp{server, …}`.
//! - MCP tools are named `<server>_<tool>` with the server sanitized to
//!   `[A-Za-z0-9_-]` (`core/src/tool/mcp.ts`), so a server name containing
//!   `_` is ambiguous: the split uses the live `GET /api/mcp` server names
//!   (longest match wins). Without a match the call stays generic.
//! - OpenCode 1.x names (`bash`, `todowrite`, `list`, …) are gone with the
//!   1.x wire (PD-15); a 1.x record renders through the generic card.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use super::*;

/// Plain (un-namespaced) tool names a 2.0.22 server registers itself.
const BUILTIN_TOOLS: &[&str] = &[
    "shell",
    "read",
    "write",
    "edit",
    "patch",
    "glob",
    "grep",
    "webfetch",
    "websearch",
    "question",
    "skill",
    "subagent",
    "execute",
];

/// The namespace of OpenCode's own namespaced tools (`opencode_models`, …).
const OPENCODE_NAMESPACE: &str = "opencode_";

/// First-seen unknown tool names are logged once, up to this many per process.
const MAX_UNKNOWN_TOOL_NAMES: usize = 64;

/// The chip a native call opens, with its stable id.
pub(super) struct ToolChip {
    pub(super) id: String,
    pub(super) call: ToolCall,
}

/// Type a native tool invocation into one chip or, for a multi-file patch,
/// one chip per file operation. The first chip always uses `call_id`.
pub(super) fn tool_chips(
    call_id: &str,
    name: &str,
    input: &Value,
    mcp_servers: &[String],
) -> Vec<ToolChip> {
    if name == "patch" {
        let files = patch_files(
            input
                .get("patchText")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        if !files.is_empty() {
            return files
                .iter()
                .enumerate()
                .map(|(index, file)| ToolChip {
                    id: chip_id(call_id, index),
                    call: file.call(),
                })
                .collect();
        }
    }
    vec![ToolChip {
        id: call_id.to_owned(),
        call: oc_tool_call(name, input, mcp_servers),
    }]
}

/// Per-file chip id: the call id for the first file, `<call>#<n>` after it
/// (the Codex file-change convention the shared surfaces already handle).
pub(super) fn chip_id(call_id: &str, index: usize) -> String {
    if index == 0 {
        call_id.to_owned()
    } else {
        format!("{call_id}#{index}")
    }
}

/// Type an OpenCode 2 tool invocation (a single chip).
pub(super) fn oc_tool_call(name: &str, input: &Value, mcp_servers: &[String]) -> ToolCall {
    let s = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| input.get(*k))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let generic = || ToolCall::Unknown {
        name: name.to_owned(),
        input: (!input.is_null()).then(|| input.clone()),
    };
    match name {
        "shell" => ToolCall::Exec {
            command: s(&["command"]).unwrap_or_default(),
        },
        "read" => ToolCall::ReadFile {
            path: s(&["path", "filePath"]).unwrap_or_default(),
        },
        "write" => ToolCall::WriteFile {
            path: s(&["path", "filePath"]).unwrap_or_default(),
            content: s(&["content"]),
        },
        "edit" => ToolCall::EditFile {
            path: s(&["path", "filePath"]).unwrap_or_default(),
            old_string: s(&["oldString"]),
            new_string: s(&["newString"]),
        },
        "patch" => ToolCall::ApplyPatch { path: None },
        "grep" => ToolCall::Search {
            pattern: s(&["pattern"]).unwrap_or_default(),
            path: s(&["path"]),
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
        // The genus-gated spawn naming every driver shares. `task` is the
        // 1.x spelling the subagent package still keys its binding on.
        "subagent" | "task" => ToolCall::Unknown {
            name: s(&["description"])
                .map(|d| format!("Agent: {d}"))
                .unwrap_or_else(|| "Agent".into()),
            input: (!input.is_null()).then(|| input.clone()),
        },
        "question" | "skill" | "execute" => generic(),
        "opencode_read_mcp_resource" | "opencode_list_mcp_resources" => match s(&["server"]) {
            Some(server) => ToolCall::Mcp {
                server,
                tool: name.trim_start_matches(OPENCODE_NAMESPACE).to_owned(),
                input: (!input.is_null()).then(|| input.clone()),
            },
            None => generic(),
        },
        _ if name.starts_with(OPENCODE_NAMESPACE) => generic(),
        _ => match mcp_split(name, mcp_servers) {
            Some((server, tool)) => ToolCall::Mcp {
                server,
                tool,
                input: (!input.is_null()).then(|| input.clone()),
            },
            None => {
                note_unknown_tool(name, input);
                generic()
            }
        },
    }
}

/// Whether typing this tool name can depend on the live MCP server list.
pub(super) fn needs_mcp_servers(name: &str) -> bool {
    !name.is_empty()
        && name.contains('_')
        && !name.starts_with(OPENCODE_NAMESPACE)
        && !BUILTIN_TOOLS.contains(&name)
}

/// OpenCode's MCP namespace for a configured server name
/// (`core/src/tool/mcp.ts` `namespace`).
fn mcp_namespace(server: &str) -> String {
    server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Split `<namespace>_<tool>` against the known server names; the longest
/// namespace wins, so `a_b` beats `a` for `a_b_tool`.
pub(super) fn mcp_split(name: &str, servers: &[String]) -> Option<(String, String)> {
    servers
        .iter()
        .filter_map(|server| {
            let namespace = mcp_namespace(server);
            let tool = name.strip_prefix(namespace.as_str())?.strip_prefix('_')?;
            (!namespace.is_empty() && !tool.is_empty())
                .then(|| (namespace.len(), server.clone(), tool.to_owned()))
        })
        .max_by_key(|(len, _, _)| *len)
        .map(|(_, server, tool)| (server, tool))
}

impl Server {
    /// Configured MCP server names for a location (`GET /api/mcp`, OC-06's
    /// `<server>_<tool>` disambiguation). Bounded; failures degrade to an
    /// empty list so typing never blocks a turn.
    pub(super) async fn mcp_server_names(&self, directory: Option<&str>) -> Vec<String> {
        let response = self
            .exchange(
                reqwest::Method::GET,
                "/api/mcp",
                directory,
                None,
                Some(MCP_LIST_TIMEOUT),
            )
            .await;
        let Ok((status, body)) = response else {
            return Vec::new();
        };
        if !status.is_success() {
            return Vec::new();
        }
        let value = serde_json::from_str::<Value>(&body)
            .map(unwrap_data)
            .unwrap_or(Value::Null);
        value
            .as_array()
            .map(|servers| {
                servers
                    .iter()
                    .filter_map(|server| server.get("name").and_then(Value::as_str))
                    .filter(|name| !name.is_empty())
                    .take(256)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Bound for the MCP server lookup on the event path.
const MCP_LIST_TIMEOUT: Duration = Duration::from_secs(2);

/// One file operation in a `patch` call's `patchText`
/// (`*** Add File:` / `*** Update File:` [+ `*** Move to:`] / `*** Delete File:`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PatchFile {
    pub(super) op: PatchOp,
    pub(super) path: String,
    pub(super) move_to: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchOp {
    Add,
    Update,
    Delete,
}

impl PatchFile {
    /// The file the operation leaves behind (the move target when moved).
    pub(super) fn target(&self) -> &str {
        self.move_to.as_deref().unwrap_or(&self.path)
    }

    fn call(&self) -> ToolCall {
        match self.op {
            PatchOp::Add => ToolCall::WriteFile {
                path: self.target().to_owned(),
                content: None,
            },
            PatchOp::Update => ToolCall::EditFile {
                path: self.target().to_owned(),
                old_string: None,
                new_string: None,
            },
            PatchOp::Delete => ToolCall::ApplyPatch {
                path: Some(self.path.clone()),
            },
        }
    }
}

/// File operations of a patch, in patch order (the order the tool applies
/// them and reports their diffs in). Malformed text yields no operations.
pub(super) fn patch_files(patch: &str) -> Vec<PatchFile> {
    let mut files: Vec<PatchFile> = Vec::new();
    for line in patch.lines() {
        let line = line.trim_end();
        let header = |prefix: &str| {
            line.strip_prefix(prefix)
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
        };
        if let Some(path) = header("*** Add File: ") {
            files.push(PatchFile {
                op: PatchOp::Add,
                path,
                move_to: None,
            });
        } else if let Some(path) = header("*** Delete File: ") {
            files.push(PatchFile {
                op: PatchOp::Delete,
                path,
                move_to: None,
            });
        } else if let Some(path) = header("*** Update File: ") {
            files.push(PatchFile {
                op: PatchOp::Update,
                path,
                move_to: None,
            });
        } else if let Some(target) = header("*** Move to: ")
            && let Some(last) = files.last_mut()
            && last.op == PatchOp::Update
            && last.move_to.is_none()
        {
            last.move_to = Some(target);
        }
        if files.len() >= MAX_PATCH_FILES {
            break;
        }
    }
    files
}

/// Per-file chips are bounded; a larger patch keeps its first files typed.
const MAX_PATCH_FILES: usize = 256;

/// Diagnostic label: control characters stripped, length bounded.
pub(super) fn diagnostic_label(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(80).collect()
}

/// Bounded diagnostic for a tool outside the known vocabulary (a plugin
/// tool, an MCP tool whose server is unknown, or a newer built-in): the
/// name and argument field names only, never values; once per name.
fn note_unknown_tool(name: &str, input: &Value) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let Ok(mut seen) = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock() else {
        return;
    };
    let label = diagnostic_label(name);
    if seen.len() >= MAX_UNKNOWN_TOOL_NAMES || !seen.insert(label.clone()) {
        return;
    }
    let fields: Vec<String> = input
        .as_object()
        .map(|fields| {
            fields
                .keys()
                .take(16)
                .map(|k| diagnostic_label(k))
                .collect()
        })
        .unwrap_or_default();
    tracing::info!(
        target: "luna_harness::opencode",
        tool = %label,
        fields = ?fields,
        "OpenCode tool outside the known vocabulary; rendered as a generic card"
    );
}
