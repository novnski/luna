//! Codex app-server notification/item → [`AgentEvent`] mapping, ported from
//! codex.ts's `mapItem`/notification switch.
//!
//! Tolerant by construction: both field spellings the app server has shipped
//! (`delta`/`textDelta`, `exitCode`/`exit_code`, camelCase/snake_case item
//! types) are accepted, and unknown item types map to nothing.

use std::collections::{HashMap, VecDeque};

use luna_proto::{
    AgentEvent, ComputerUseStatus, ContextOperationStatus, DoneStatus, TodoItem, ToolCall, ToolDiff,
};
use serde_json::Value;

/// A command can print megabytes. Keep the host journal and lazy RPC response
/// bounded while preserving ordinary command output verbatim.
const COMMAND_OUTPUT_MAX_BYTES: usize = 64 * 1024;

fn command_output(item: &Value) -> Option<String> {
    let output = field(item, &["aggregatedOutput", "aggregated_output"])
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())?;
    if output.len() <= COMMAND_OUTPUT_MAX_BYTES {
        return Some(output.to_owned());
    }
    let mut end = COMMAND_OUTPUT_MAX_BYTES;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!(
        "{}\n… [command output truncated by LUNA after 64 KiB]",
        &output[..end]
    ))
}

fn bounded_text(text: &str) -> String {
    if text.len() <= COMMAND_OUTPUT_MAX_BYTES {
        return text.to_owned();
    }
    let mut end = COMMAND_OUTPUT_MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… [tool output truncated by LUNA after 64 KiB]",
        &text[..end]
    )
}

fn command_call(item: &Value) -> ToolCall {
    let command = str_field(item, &["command"]);
    let actions = item
        .get("commandActions")
        .or_else(|| item.get("command_actions"))
        .and_then(Value::as_array);
    match actions.map(Vec::as_slice) {
        Some([action]) => match action.get("type").and_then(Value::as_str) {
            Some("read") => action
                .get("path")
                .and_then(Value::as_str)
                .map(|path| ToolCall::ReadFile {
                    path: path.to_owned(),
                })
                .unwrap_or(ToolCall::Exec { command }),
            Some("search") => action
                .get("query")
                .and_then(Value::as_str)
                .map(|pattern| ToolCall::Search {
                    pattern: pattern.to_owned(),
                    path: action
                        .get("path")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .unwrap_or(ToolCall::Exec { command }),
            Some("listFiles") => ToolCall::Glob {
                pattern: action
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("workspace")
                    .to_owned(),
            },
            _ => ToolCall::Exec { command },
        },
        _ => ToolCall::Exec { command },
    }
}

fn web_search_call(item: &Value) -> ToolCall {
    let query = str_field(item, &["query"]);
    match item.get("action") {
        Some(action) if action.get("type").and_then(Value::as_str) == Some("openPage") => {
            let url = str_field(action, &["url"]);
            ToolCall::WebFetch { url, prompt: None }
        }
        Some(action) if action.get("type").and_then(Value::as_str) == Some("findInPage") => {
            let url = str_field(action, &["url"]);
            let pattern = str_field(action, &["pattern"]);
            ToolCall::WebSearch {
                query: format!("Find {pattern} in {url}"),
            }
        }
        Some(action) if query.is_empty() => {
            let queries = action
                .get("queries")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .unwrap_or_default();
            ToolCall::WebSearch { query: queries }
        }
        _ => ToolCall::WebSearch { query },
    }
}

fn web_search_output(item: &Value) -> Option<String> {
    let results = item.get("results").filter(|value| !value.is_null())?;
    let text = serde_json::to_string_pretty(results).ok()?;
    (!text.is_empty() && text != "[]").then(|| bounded_text(&text))
}

fn file_change_events(phase: Phase, id: String, item: &Value, failed: bool) -> Vec<AgentEvent> {
    let Some(changes) = item.get("changes").and_then(Value::as_array) else {
        return tool_lifecycle(phase, id, ToolCall::ApplyPatch { path: None }, failed);
    };
    if changes.is_empty() {
        return tool_lifecycle(phase, id, ToolCall::ApplyPatch { path: None }, failed);
    }
    if phase == Phase::Started && changes.len() > 1 {
        return vec![AgentEvent::ToolCall {
            id,
            call: ToolCall::ApplyPatch { path: None },
        }];
    }
    let mut events = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let path = str_field(change, &["path"]);
        let kind = change
            .get("kind")
            .and_then(|value| value.as_str().or_else(|| value.get("type")?.as_str()))
            .unwrap_or("update")
            .to_owned();
        let part_id = if index == 0 {
            id.clone()
        } else {
            format!("{id}#{index}")
        };
        let call = file_change_call(&[(path.clone(), kind.clone())]);
        events.push(AgentEvent::ToolCall {
            id: part_id.clone(),
            call,
        });
        if phase == Phase::Completed {
            let patch = change
                .get("diff")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(bounded_text);
            let diff = patch.map(|text| {
                let is_patch = text.starts_with("diff --git ") || text.starts_with("@@ ");
                let (old_text, new_text, unified_diff) = match (kind.as_str(), is_patch) {
                    ("add", false) => (None, text, None),
                    ("delete", false) => (Some(text), String::new(), None),
                    _ => (None, String::new(), Some(text)),
                };
                ToolDiff {
                    path,
                    old_text,
                    new_text,
                    unified_diff,
                }
            });
            events.push(AgentEvent::ToolResult {
                id: part_id,
                is_error: failed,
                output: None,
                diff,
            });
        }
    }
    events
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Started,
    Completed,
}

#[derive(Clone)]
struct ActiveComputerUse {
    thread_id: String,
    turn_id: String,
    call: ToolCall,
}

/// Computer use is observed from a provider-owned item, never inferred from
/// an installed CLI, an MCP config entry, or an app name alone.
#[derive(Default)]
pub(crate) struct ComputerUseStream {
    active: HashMap<String, ActiveComputerUse>,
    settled: VecDeque<(String, String, String)>,
    task_id: Option<String>,
}

impl ComputerUseStream {
    pub(crate) fn for_task(task_id: Option<&str>) -> Self {
        Self {
            task_id: task_id.filter(|value| !value.is_empty()).map(str::to_owned),
            ..Self::default()
        }
    }
    pub(crate) fn map_item(
        &mut self,
        phase: Phase,
        root_thread: &str,
        active_turn: Option<&str>,
        params: &Value,
        item: &Value,
    ) -> Option<Vec<AgentEvent>> {
        if !matches!(item_type(item), "mcpToolCall" | "mcp_tool_call") {
            return None;
        }
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let thread = notification_thread_id("item/started", params);
        let turn = field(params, &["turnId", "turn_id"]).and_then(Value::as_str);
        let settled = thread.as_deref().zip(turn).is_some_and(|(thread, turn)| {
            self.settled
                .iter()
                .any(|old| old.0 == thread && old.1 == turn && old.2 == id)
        });
        if !is_computer_use(item) && !self.active.contains_key(id) && !settled {
            return None;
        }
        let Some(thread) = thread else {
            return Some(Vec::new());
        };
        let Some(turn) = turn else {
            return Some(Vec::new());
        };
        if thread != root_thread || active_turn != Some(turn) || id.is_empty() || settled {
            return Some(Vec::new());
        }
        match phase {
            Phase::Started => {
                if self.active.contains_key(id) || self.active.len() >= 128 {
                    return Some(Vec::new());
                }
                let call = computer_use_call(
                    item,
                    self.task_id.as_deref(),
                    &thread,
                    turn,
                    ComputerUseStatus::Running,
                );
                self.active.insert(
                    id.to_owned(),
                    ActiveComputerUse {
                        thread_id: thread,
                        turn_id: turn.to_owned(),
                        call: call.clone(),
                    },
                );
                Some(vec![AgentEvent::ToolCall {
                    id: id.to_owned(),
                    call,
                }])
            }
            Phase::Completed => {
                if self
                    .active
                    .get(id)
                    .is_some_and(|active| active.thread_id != thread || active.turn_id != turn)
                {
                    return Some(Vec::new());
                }
                // The provider may attach its computer-use surface only to
                // the completed item. Its live thread/turn still identifies
                // the call without inventing a start event.
                if !self.active.contains_key(id) && !is_computer_use(item) {
                    return Some(Vec::new());
                }
                let failed = matches!(
                    item.get("status").and_then(Value::as_str),
                    Some("failed" | "declined" | "cancelled" | "canceled")
                ) || item.get("error").is_some_and(|error| !error.is_null());
                let status = if failed {
                    ComputerUseStatus::ProviderFailed
                } else if item.get("status").and_then(Value::as_str) == Some("completed") {
                    ComputerUseStatus::ProviderCompleted
                } else {
                    ComputerUseStatus::Unknown
                };
                let mut call =
                    computer_use_call(item, self.task_id.as_deref(), &thread, turn, status);
                if let Some(active) = self.active.get(id)
                    && let (
                        ToolCall::ComputerUse {
                            app,
                            screenshot_observed,
                            ..
                        },
                        ToolCall::ComputerUse {
                            app: previous_app,
                            screenshot_observed: previous_screenshot,
                            ..
                        },
                    ) = (&mut call, &active.call)
                {
                    if app.is_none() {
                        *app = previous_app.clone();
                    }
                    *screenshot_observed |= *previous_screenshot;
                }
                self.active.remove(id);
                self.remember(&thread, turn, id);
                Some(vec![
                    AgentEvent::ToolCall {
                        id: id.to_owned(),
                        call,
                    },
                    AgentEvent::ToolResult {
                        id: id.to_owned(),
                        is_error: failed,
                        output: None,
                        diff: None,
                    },
                ])
            }
        }
    }

    pub(crate) fn unknown_for_turn(&mut self, thread: &str, turn: &str) -> Vec<AgentEvent> {
        let ids: Vec<String> = self
            .active
            .iter()
            .filter(|(_, active)| active.thread_id == thread && active.turn_id == turn)
            .map(|(id, _)| id.clone())
            .collect();
        self.unknown_ids(ids)
    }

    pub(crate) fn unknown_before_turn(&mut self, thread: &str, next_turn: &str) -> Vec<AgentEvent> {
        let ids: Vec<String> = self
            .active
            .iter()
            .filter(|(_, active)| active.thread_id == thread && active.turn_id != next_turn)
            .map(|(id, _)| id.clone())
            .collect();
        self.unknown_ids(ids)
    }

    pub(crate) fn take_unknown(&mut self) -> Vec<AgentEvent> {
        self.unknown_ids(self.active.keys().cloned().collect())
    }

    fn unknown_ids(&mut self, ids: Vec<String>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for id in ids {
            if let Some(mut active) = self.active.remove(&id) {
                if let ToolCall::ComputerUse { status, .. } = &mut active.call {
                    *status = ComputerUseStatus::Unknown;
                }
                self.remember(&active.thread_id, &active.turn_id, &id);
                events.push(AgentEvent::ToolCall {
                    id: id.clone(),
                    call: active.call,
                });
                events.push(AgentEvent::ToolResult {
                    id,
                    is_error: false,
                    output: None,
                    diff: None,
                });
            }
        }
        events
    }

    fn remember(&mut self, thread: &str, turn: &str, id: &str) {
        self.settled
            .push_back((thread.to_owned(), turn.to_owned(), id.to_owned()));
        if self.settled.len() > 256 {
            self.settled.pop_front();
        }
    }
}

pub(crate) fn is_computer_use(item: &Value) -> bool {
    item.get("server").and_then(Value::as_str) == Some("computer-use")
        || item
            .pointer("/result/_meta/codex~1toolSurface/kind")
            .and_then(Value::as_str)
            == Some("computerUse")
}

fn computer_use_call(
    item: &Value,
    task_id: Option<&str>,
    thread: &str,
    turn: &str,
    status: ComputerUseStatus,
) -> ToolCall {
    let app = item
        .pointer("/result/_meta/codex~1toolSurface/app/displayName")
        .or_else(|| item.pointer("/result/_meta/codex~1toolSurface/app/appId"))
        .or_else(|| item.pointer("/arguments/app"))
        .or_else(|| item.pointer("/arguments/appName"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.chars().take(120).collect());
    let screenshot_observed = item
        .pointer("/result/content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content.iter().any(|entry| {
                matches!(
                    entry.get("type").and_then(Value::as_str),
                    Some("image" | "image/png" | "image/jpeg")
                )
            })
        });
    ToolCall::ComputerUse {
        action: item
            .get("tool")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("unreported action")
            .chars()
            .take(80)
            .collect(),
        app,
        task_id: task_id.map(str::to_owned),
        thread_id: thread.to_owned(),
        turn_id: turn.to_owned(),
        status,
        screenshot_observed,
    }
}

#[cfg(test)]
mod computer_use_tests {
    use super::*;
    use serde_json::json;

    fn item(status: &str) -> Value {
        json!({
            "type":"mcpToolCall", "id":"computer-1", "server":"computer-use",
            "tool":"click", "arguments":{"app":"Finder", "x":10},
            "status":status,
            "result":{"_meta":{"codex/toolSurface":{"kind":"computerUse"}},
                "content":[{"type":"image","data":"private pixels"}]}
        })
    }

    #[test]
    fn exact_provider_turn_lifecycle_and_evidence_without_payload() {
        let mut stream = ComputerUseStream::default();
        let params = json!({"threadId":"provider-1","turnId":"turn-1"});
        assert!(
            stream
                .map_item(
                    Phase::Started,
                    "provider-1",
                    Some("turn-1"),
                    &params,
                    &item("inProgress")
                )
                .is_some()
        );
        let events = stream
            .map_item(
                Phase::Completed,
                "provider-1",
                Some("turn-1"),
                &params,
                &item("completed"),
            )
            .unwrap();
        assert!(
            matches!(&events[0], AgentEvent::ToolCall { call: ToolCall::ComputerUse { app: Some(app), status: ComputerUseStatus::ProviderCompleted, screenshot_observed: true, thread_id, turn_id, .. }, .. } if app == "Finder" && thread_id == "provider-1" && turn_id == "turn-1")
        );
        assert!(matches!(
            &events[1],
            AgentEvent::ToolResult {
                is_error: false,
                output: None,
                ..
            }
        ));
        assert!(!format!("{events:?}").contains("private pixels"));
        assert!(
            stream
                .map_item(
                    Phase::Completed,
                    "provider-1",
                    Some("turn-1"),
                    &params,
                    &item("completed")
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn wrong_target_and_late_results_cannot_settle_an_active_call() {
        let mut stream = ComputerUseStream::default();
        let params = json!({"threadId":"provider-1","turnId":"turn-1"});
        assert!(
            stream
                .map_item(
                    Phase::Started,
                    "provider-1",
                    Some("turn-1"),
                    &json!({"threadId":"foreign","turnId":"turn-1"}),
                    &item("inProgress")
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            stream
                .map_item(
                    Phase::Started,
                    "provider-1",
                    Some("turn-1"),
                    &params,
                    &item("inProgress")
                )
                .unwrap()
                .len()
                == 1
        );
        assert!(
            stream
                .map_item(
                    Phase::Completed,
                    "provider-1",
                    Some("turn-2"),
                    &params,
                    &item("completed")
                )
                .unwrap()
                .is_empty()
        );
        let unknown = stream.unknown_for_turn("provider-1", "turn-1");
        assert!(matches!(
            &unknown[0],
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse {
                    status: ComputerUseStatus::Unknown,
                    ..
                },
                ..
            }
        ));
        assert!(
            stream
                .map_item(
                    Phase::Completed,
                    "provider-1",
                    Some("turn-1"),
                    &params,
                    &item("completed")
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn no_observed_computer_use_remains_generic_and_disconnect_is_unknown() {
        let mut stream = ComputerUseStream::default();
        let params = json!({"threadId":"provider-1","turnId":"turn-1"});
        let ordinary = json!({"type":"mcpToolCall","id":"ordinary","server":"other","tool":"read"});
        assert!(
            stream
                .map_item(
                    Phase::Started,
                    "provider-1",
                    Some("turn-1"),
                    &params,
                    &ordinary
                )
                .is_none()
        );
        stream.map_item(
            Phase::Started,
            "provider-1",
            Some("turn-1"),
            &params,
            &item("inProgress"),
        );
        let unknown = stream.take_unknown();
        assert!(matches!(
            &unknown[0],
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse {
                    status: ComputerUseStatus::Unknown,
                    ..
                },
                ..
            }
        ));
        assert!(stream.take_unknown().is_empty());
    }

    #[test]
    fn completion_only_surface_is_observed_on_current_turn() {
        let mut stream = ComputerUseStream::for_task(Some("task-1"));
        let params = json!({"threadId":"provider-1","turnId":"turn-1"});
        let mut completed = item("completed");
        completed["server"] = json!("node_repl");
        let events = stream
            .map_item(
                Phase::Completed,
                "provider-1",
                Some("turn-1"),
                &params,
                &completed,
            )
            .unwrap();
        assert!(
            matches!(&events[0], AgentEvent::ToolCall { call: ToolCall::ComputerUse { task_id: Some(task_id), status: ComputerUseStatus::ProviderCompleted, .. }, .. } if task_id == "task-1")
        );
        assert!(
            stream
                .map_item(
                    Phase::Completed,
                    "provider-1",
                    Some("turn-1"),
                    &params,
                    &completed
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn settled_item_id_may_be_reused_on_a_later_turn() {
        let mut stream = ComputerUseStream::default();
        let first = json!({"threadId":"provider-1","turnId":"turn-1"});
        let second = json!({"threadId":"provider-1","turnId":"turn-2"});
        assert_eq!(
            stream
                .map_item(
                    Phase::Started,
                    "provider-1",
                    Some("turn-1"),
                    &first,
                    &item("inProgress")
                )
                .unwrap()
                .len(),
            1
        );
        assert_eq!(stream.unknown_for_turn("provider-1", "turn-1").len(), 2);
        assert_eq!(
            stream
                .map_item(
                    Phase::Started,
                    "provider-1",
                    Some("turn-2"),
                    &second,
                    &item("inProgress")
                )
                .unwrap()
                .len(),
            1
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveContextCompaction {
    operation_id: String,
    thread_id: String,
    turn_id: String,
    item_id: String,
}

/// Correlates the canonical 0.154 `contextCompaction` item lifecycle. The
/// app-server notification envelope owns thread/turn identity while the item
/// owns the stable id; all three must match before completion is surfaced.
#[derive(Default)]
pub(crate) struct ContextCompactionStream {
    active: Option<ActiveContextCompaction>,
    settled: VecDeque<(String, String, String)>,
}

impl ContextCompactionStream {
    pub(crate) fn map_item(
        &mut self,
        phase: Phase,
        root_thread_id: &str,
        active_turn_id: Option<&str>,
        params: &Value,
        item: &Value,
    ) -> Option<AgentEvent> {
        if !matches!(item_type(item), "contextCompaction" | "context_compaction") {
            return None;
        }
        let thread_id = notification_thread_id("item/started", params)?;
        let turn_id = field(params, &["turnId", "turn_id"])
            .and_then(Value::as_str)
            .unwrap_or("");
        let item_id = str_field(item, &["id"]);
        if thread_id != root_thread_id || turn_id.is_empty() || item_id.is_empty() {
            return None;
        }
        let operation_id = format!("codex-auto:{thread_id}:{turn_id}:{item_id}");
        match phase {
            Phase::Started => {
                if active_turn_id != Some(turn_id)
                    || self.settled.iter().any(|settled| {
                        settled.0 == thread_id && settled.1 == turn_id && settled.2 == item_id
                    })
                {
                    return None;
                }
                if self.active.as_ref().is_some_and(|active| {
                    active.thread_id == thread_id
                        && active.turn_id == turn_id
                        && active.item_id == item_id
                }) {
                    return None;
                }
                if self.active.is_some() {
                    return None;
                }
                self.active = Some(ActiveContextCompaction {
                    operation_id: operation_id.clone(),
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.to_string(),
                    item_id,
                });
                Some(AgentEvent::ContextCompaction {
                    operation_id,
                    status: ContextOperationStatus::Running,
                    thread_id,
                    turn_id: turn_id.to_string(),
                })
            }
            Phase::Completed => {
                let active = self.active.as_ref()?;
                if active.thread_id != thread_id
                    || active.turn_id != turn_id
                    || active.item_id != item_id
                {
                    return None;
                }
                let active = self.active.take().unwrap();
                self.remember_settled(&active);
                Some(AgentEvent::ContextCompaction {
                    operation_id: active.operation_id,
                    status: ContextOperationStatus::Completed,
                    thread_id: active.thread_id,
                    turn_id: active.turn_id,
                })
            }
        }
    }

    pub(crate) fn unknown_for_turn(
        &mut self,
        root_thread_id: &str,
        turn_id: &str,
    ) -> Option<AgentEvent> {
        let active = self.active.as_ref()?;
        if active.thread_id != root_thread_id || active.turn_id != turn_id {
            return None;
        }
        self.take_unknown()
    }

    pub(crate) fn unknown_before_turn(
        &mut self,
        root_thread_id: &str,
        next_turn_id: &str,
    ) -> Option<AgentEvent> {
        let active = self.active.as_ref()?;
        if active.thread_id != root_thread_id || active.turn_id == next_turn_id {
            return None;
        }
        self.take_unknown()
    }

    pub(crate) fn take_unknown(&mut self) -> Option<AgentEvent> {
        let active = self.active.take()?;
        self.remember_settled(&active);
        Some(AgentEvent::ContextCompaction {
            operation_id: active.operation_id,
            status: ContextOperationStatus::Unknown,
            thread_id: active.thread_id,
            turn_id: active.turn_id,
        })
    }

    fn remember_settled(&mut self, active: &ActiveContextCompaction) {
        self.settled.push_back((
            active.thread_id.clone(),
            active.turn_id.clone(),
            active.item_id.clone(),
        ));
        while self.settled.len() > 32 {
            self.settled.pop_front();
        }
    }
}

fn field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k))
}

fn str_field(v: &Value, keys: &[&str]) -> String {
    field(v, keys)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Delta text under either spelling the app server has used
/// (`delta` on agentMessage, `textDelta` on some reasoning builds).
pub(crate) fn delta_text(params: &Value) -> Option<String> {
    field(params, &["delta", "textDelta"])
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Codex streams independent reasoning items and indexed parts without text
/// separators. Preserve those boundaries before the document folds deltas
/// together; otherwise adjacent Markdown headings become `**one****two**`.
/// One instance belongs to one thread, so child activity cannot split a
/// parent's in-flight paragraph.
#[derive(Default)]
pub(crate) struct ReasoningStream {
    last_part: Option<(String, bool, u64)>,
    announced_summary: Option<(String, u64)>,
    trailing_newlines: usize,
}

impl ReasoningStream {
    pub(crate) fn map(&mut self, method: &str, params: &Value) -> Vec<AgentEvent> {
        let id = item_id(params);
        let summary = method != "item/reasoning/textDelta";
        let index = field(
            params,
            if summary {
                &["summaryIndex", "summary_index"]
            } else {
                &["contentIndex", "content_index"]
            },
        )
        .and_then(Value::as_u64)
        .or_else(|| {
            self.announced_summary
                .as_ref()
                .filter(|(item, _)| summary && item == &id)
                .map(|(_, index)| *index)
        })
        .unwrap_or(0);
        if method == "item/reasoning/summaryPartAdded" {
            self.announced_summary = Some((id, index));
            return Vec::new();
        }
        let Some(text) = delta_text(params) else {
            return Vec::new();
        };
        let part = (id, summary, index);
        let mut events = Vec::new();
        if self.last_part.as_ref().is_some_and(|last| last != &part) {
            let leading_newlines = text.chars().take_while(|&c| c == '\n').count();
            let missing = 2usize.saturating_sub(self.trailing_newlines + leading_newlines);
            if missing > 0 {
                events.push(AgentEvent::ReasoningDelta {
                    text: "\n".repeat(missing),
                });
                self.trailing_newlines += missing;
            }
        }
        let trailing = text.chars().rev().take_while(|&c| c == '\n').count();
        self.trailing_newlines = if trailing == text.len() {
            self.trailing_newlines + trailing
        } else {
            trailing
        };
        self.last_part = Some(part);
        events.push(AgentEvent::ReasoningDelta { text });
        events
    }
}

pub(crate) fn item_id(params: &Value) -> String {
    str_field(params, &["itemId", "item_id"])
}

/// `params.turn.id` on the turn/* lifecycle notifications.
pub(crate) fn turn_id(params: &Value) -> String {
    params
        .get("turn")
        .map(|t| str_field(t, &["id"]))
        .unwrap_or_default()
}

/// `params.turn.error.message` (turn/completed carries an optional error;
/// turn/failed always should).
pub(crate) fn turn_error_message(params: &Value) -> Option<String> {
    params
        .get("turn")
        .and_then(|t| t.get("error"))
        .filter(|e| !e.is_null())
        .map(|e| {
            let msg = str_field(e, &["message"]);
            if msg.is_empty() { e.to_string() } else { msg }
        })
}

/// `thread/tokenUsage/updated` → a [`AgentEvent::Usage`] snapshot of the LAST
/// turn's tokens (held by the session loop, emitted before `Done`).
pub(crate) fn usage_event(params: &Value) -> Option<AgentEvent> {
    let last = field(params, &["tokenUsage", "token_usage"])?.get("last")?;
    let count = |keys: &[&str]| {
        field(last, keys)
            .and_then(Value::as_u64)
            .unwrap_or_default()
    };
    Some(AgentEvent::Usage {
        input_tokens: count(&["inputTokens", "input_tokens"]),
        output_tokens: count(&["outputTokens", "output_tokens"]),
    })
}

pub(crate) fn context_usage_event(params: &Value) -> Option<AgentEvent> {
    let usage = field(params, &["tokenUsage", "token_usage"])?;
    let last = usage.get("last");
    let tokens = last
        .and_then(|last| field(last, &["totalTokens", "total_tokens"]).and_then(Value::as_u64))
        .or_else(|| {
            let last = last?;
            let input = field(last, &["inputTokens", "input_tokens"])?.as_u64()?;
            Some(
                input.saturating_add(
                    field(last, &["outputTokens", "output_tokens"])
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                ),
            )
        });
    let window = field(usage, &["modelContextWindow", "model_context_window"])
        .and_then(Value::as_u64)
        .filter(|n| *n > 0);
    (tokens.is_some() || window.is_some()).then_some(AgentEvent::ContextUsage { tokens, window })
}

/// Tool-shaped Codex items must always close the lifecycle they open: started
/// opens the ToolCall, completed refreshes its metadata and resolves the same
/// stable id (port of codex.ts `toolLifecycle`).
fn tool_lifecycle(phase: Phase, id: String, call: ToolCall, is_error: bool) -> Vec<AgentEvent> {
    match phase {
        Phase::Started => vec![AgentEvent::ToolCall { id, call }],
        Phase::Completed => vec![
            AgentEvent::ToolCall {
                id: id.clone(),
                call,
            },
            AgentEvent::ToolResult {
                id,
                is_error,
                output: None,
                diff: None,
            },
        ],
    }
}

/// A `fileChange` item's `changes` array reduced to the typed [`ToolCall`] the
/// UI renders: a lone `add` is a file write, a lone `update` an edit, anything
/// else (deletes, multi-file changes) a patch.
fn file_change_call(changes: &[(String, String)]) -> ToolCall {
    match changes {
        [(path, kind)] if kind == "add" => ToolCall::WriteFile {
            path: path.clone(),
            content: None,
        },
        [(path, kind)] if kind == "update" => ToolCall::EditFile {
            path: path.clone(),
            old_string: None,
            new_string: None,
        },
        [(path, _)] => ToolCall::ApplyPatch {
            path: Some(path.clone()),
        },
        _ => ToolCall::ApplyPatch { path: None },
    }
}

pub(crate) fn item_type(item: &Value) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or("")
}

pub(super) fn is_collab_spawn(item: &Value) -> bool {
    matches!(
        item_type(item),
        "collabAgentToolCall" | "collab_agent_tool_call"
    ) && matches!(
        item.get("tool").and_then(Value::as_str),
        Some("spawnAgent" | "spawn_agent")
    )
}

/// A v1 spawn names its child in the completed result. Other collaboration
/// tools can address several receivers, but only a spawn owns a transcript.
pub(super) fn collab_spawn_child(item: &Value) -> Option<&str> {
    if !is_collab_spawn(item)
        || matches!(
            item.get("status").and_then(Value::as_str),
            Some("failed" | "errored")
        )
    {
        return None;
    }
    let receivers = field(item, &["receiverThreadIds", "receiver_thread_ids"])?.as_array()?;
    match receivers.as_slice() {
        [child] => child.as_str().filter(|id| !id.is_empty()),
        _ => None,
    }
}

/// Map one `item/started` or `item/completed` payload's item to events.
/// `agentMessage` and `reasoning` flow through their delta channels and are
/// handled by the session loop, not here.
pub(crate) fn map_item(phase: Phase, item: &Value) -> Vec<AgentEvent> {
    let id = str_field(item, &["id"]);
    let status = str_field(item, &["status"]);
    match item_type(item) {
        "imageGeneration" | "image_generation" => {
            let call = ToolCall::Unknown {
                name: "Generate image".into(),
                input: None,
            };
            if phase == Phase::Started {
                return vec![AgentEvent::ToolCall { id, call }];
            }
            // `result` can be megabytes of inline media. Never inspect, clone,
            // log, or forward it: savedPath is the sole supported source.
            let path = str_field(item, &["savedPath", "saved_path"]);
            let failure = item.get("failure").filter(|v| !v.is_null());
            let error = if let Some(failure) = failure {
                Some(
                    if matches!(
                        failure.get("type").and_then(Value::as_str),
                        Some("usageLimitExceeded" | "usage_limit_exceeded")
                    ) {
                        "Image generation usage limit exceeded"
                    } else {
                        "Image generation failed"
                    },
                )
            } else if matches!(status.as_str(), "failed" | "cancelled" | "canceled") {
                Some("Image generation failed")
            } else if path.trim().is_empty() {
                Some("Image generation completed without a saved file")
            } else {
                None
            };
            let mut events = vec![
                AgentEvent::ToolCall {
                    id: id.clone(),
                    call,
                },
                AgentEvent::ToolResult {
                    id: id.clone(),
                    is_error: error.is_some(),
                    output: None,
                    diff: None,
                },
            ];
            if let Some(message) = error {
                events.push(AgentEvent::Error {
                    message: message.into(),
                });
            } else {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("generated.png")
                    .to_owned();
                events.push(AgentEvent::GeneratedImage {
                    id: format!("{id}:image"),
                    path,
                    name,
                    mime_type: String::new(),
                });
            }
            events
        }
        "commandExecution" | "command_execution" => match phase {
            Phase::Started => vec![AgentEvent::ToolCall {
                id,
                call: command_call(item),
            }],
            Phase::Completed => {
                let exit_code = field(item, &["exitCode", "exit_code"])
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let call = command_call(item);
                let mut events = Vec::new();
                if !matches!(call, ToolCall::Exec { .. }) {
                    events.push(AgentEvent::ToolCall {
                        id: id.clone(),
                        call,
                    });
                }
                events.push(AgentEvent::ToolResult {
                    id,
                    is_error: status == "failed" || exit_code != 0,
                    output: command_output(item),
                    diff: None,
                });
                events
            }
        },
        "fileChange" | "file_change" => {
            file_change_events(phase, id, item, status == "failed" || status == "declined")
        }
        "mcpToolCall" | "mcp_tool_call" => match phase {
            Phase::Started => {
                let input = item.get("arguments").filter(|v| !v.is_null()).cloned();
                vec![AgentEvent::ToolCall {
                    id,
                    call: ToolCall::Mcp {
                        server: str_field(item, &["server"]),
                        tool: str_field(item, &["tool"]),
                        input,
                    },
                }]
            }
            Phase::Completed => vec![AgentEvent::ToolResult {
                id,
                is_error: status == "failed",
                output: item
                    .pointer("/result/content")
                    .and_then(Value::as_array)
                    .and_then(|items| {
                        let text = items
                            .iter()
                            .filter_map(|entry| {
                                (entry.get("type").and_then(Value::as_str) == Some("text"))
                                    .then(|| entry.get("text").and_then(Value::as_str))
                                    .flatten()
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        (!text.is_empty()).then(|| bounded_text(&text))
                    }),
                diff: None,
            }],
        },
        "dynamicToolCall" | "dynamic_tool_call" => match phase {
            Phase::Started => {
                let tool = str_field(item, &["tool"]);
                if tool != "luna_status" {
                    return Vec::new();
                }
                vec![AgentEvent::ToolCall {
                    id,
                    call: ToolCall::Mcp {
                        server: "luna".into(),
                        tool,
                        input: item
                            .get("arguments")
                            .filter(|value| !value.is_null())
                            .cloned(),
                    },
                }]
            }
            Phase::Completed if str_field(item, &["tool"]) == "luna_status" => {
                vec![AgentEvent::ToolResult {
                    id,
                    is_error: status == "failed"
                        || item.get("success").and_then(Value::as_bool) == Some(false),
                    // Tool results are untrusted data. Record the outcome without
                    // copying the body into the transcript or activity index.
                    output: None,
                    diff: None,
                }]
            }
            Phase::Completed => Vec::new(),
        },
        "webSearch" | "web_search" => {
            let mut events = tool_lifecycle(phase, id, web_search_call(item), false);
            if phase == Phase::Completed {
                if let Some(AgentEvent::ToolResult { output, .. }) = events.last_mut() {
                    *output = web_search_output(item);
                }
            }
            events
        }
        "imageView" | "image_view" => {
            let path = str_field(item, &["path"]);
            let mut events = tool_lifecycle(
                phase,
                id.clone(),
                ToolCall::ViewImage { path: path.clone() },
                status == "failed",
            );
            if phase == Phase::Completed && status != "failed" && !path.is_empty() {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("image")
                    .to_owned();
                events.push(AgentEvent::GeneratedImage {
                    id: format!("{id}:view"),
                    path,
                    name,
                    mime_type: String::new(),
                });
            }
            events
        }
        "todoList" | "todo_list" => {
            let items = item
                .get("items")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|t| TodoItem {
                    text: str_field(t, &["text"]),
                    done: field(t, &["completed", "done"]).and_then(Value::as_bool) == Some(true),
                })
                .collect();
            tool_lifecycle(phase, id, ToolCall::Todo { items }, false)
        }
        "error" => vec![AgentEvent::Error {
            message: str_field(item, &["message"]),
        }],
        "collabAgentToolCall" | "collab_agent_tool_call" => {
            let tool = str_field(item, &["tool"]);
            let name = if is_collab_spawn(item) {
                "Agent".to_owned()
            } else {
                match tool.as_str() {
                    "sendInput" | "send_input" => "Send agent message".to_owned(),
                    "wait" => "Wait for agents".to_owned(),
                    "closeAgent" | "close_agent" => "Close agent".to_owned(),
                    "resumeAgent" | "resume_agent" => "Resume agent".to_owned(),
                    _ => format!("Agent control: {tool}"),
                }
            };
            tool_lifecycle(
                phase,
                id,
                ToolCall::Unknown {
                    name,
                    input: Some(item.clone()),
                },
                matches!(status.as_str(), "failed" | "errored"),
            )
        }
        // A subagent spawn/lifecycle marker on the PARENT thread (multi-agent
        // v2, codex 0.146.x): the parent-feed chip for the child thread. The
        // child's own traffic routes separately (see `route_child_notification`
        // in mod.rs); this is only the spawn tool call the chip folds from.
        "subAgentActivity" | "sub_agent_activity" => {
            // A lifecycle marker has its own item id. It is not another
            // spawn; the child's turn notifications carry its terminal state.
            if !matches!(
                item.get("kind").and_then(Value::as_str),
                Some("started" | "spawned")
            ) {
                return Vec::new();
            }
            let name = str_field(item, &["agentPath"])
                .rsplit('/')
                .find(|s| !s.is_empty())
                .map(|leaf| format!("Agent: {leaf}"))
                .unwrap_or_else(|| "Agent".to_owned());
            tool_lifecycle(
                phase,
                id,
                ToolCall::Unknown {
                    name,
                    input: Some(item.clone()),
                },
                matches!(str_field(item, &["kind"]).as_str(), "failed" | "errored"),
            )
        }
        // reasoning / agentMessage flow through delta channels; the PARENT
        // feed's userMessage items are echoes of prompts we sent (already in
        // the doc). A CHILD thread's userMessage is different — the parent
        // steering its subagent — and is mapped where child items route
        // (mod.rs child branch), not here.
        _ => Vec::new(),
    }
}

/// The text of a `userMessage` thread item. Codex builds have carried both
/// shapes: a plain `text` field and a `content` array of text blocks.
pub(crate) fn user_message_text(item: &Value) -> Option<String> {
    let text = str_field(item, &["text"]);
    if !text.trim().is_empty() {
        return Some(text);
    }
    let joined: String = item
        .get("content")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.trim().is_empty()).then_some(joined)
}

/// Per-child stream state, retained across parent turns and follow-up tasks.
/// Completion-only messages need the same text fallback as the root. Replayed
/// user items must not reopen a finished document or duplicate a steering entry.
#[derive(Default)]
pub(super) struct ChildStream {
    reasoning: ReasoningStream,
    streamed_text: std::collections::HashSet<String>,
    completed_items: std::collections::VecDeque<String>,
    completed_turns: std::collections::VecDeque<String>,
    settled: bool,
}

fn remember(ids: &mut std::collections::VecDeque<String>, id: String) -> bool {
    if id.is_empty() {
        return true;
    }
    if ids.contains(&id) {
        return false;
    }
    if ids.len() == 256 {
        ids.pop_front();
    }
    ids.push_back(id);
    true
}

impl ChildStream {
    pub(super) fn map(&mut self, child: &str, method: &str, params: &Value) -> Vec<AgentEvent> {
        if method == "thread/status/changed" {
            return match params.pointer("/status/type").and_then(Value::as_str) {
                Some("active") => vec![AgentEvent::ChildStatus {
                    status: luna_proto::ChildRuntimeStatus::Active,
                }],
                Some("idle") => vec![AgentEvent::ChildStatus {
                    status: luna_proto::ChildRuntimeStatus::Idle,
                }],
                _ => Vec::new(),
            };
        }
        if method == "turn/started" {
            let id = turn_id(params);
            if !id.is_empty() && self.completed_turns.contains(&id) {
                return Vec::new();
            }
            if self.settled {
                self.settled = false;
                // v2 followup_task starts a new child turn without echoing a
                // userMessage. Reopen the existing document without inventing
                // a user prompt or attributing an activity id as a new spawn.
                return vec![AgentEvent::Steered {
                    assistant_message_id: None,
                    next_assistant_message_id: None,
                }];
            }
            return Vec::new();
        }
        if matches!(method, "item/started" | "item/completed") {
            let item = params.get("item").unwrap_or(&Value::Null);
            let phase = if method == "item/started" {
                Phase::Started
            } else {
                Phase::Completed
            };
            if phase == Phase::Completed
                && !remember(&mut self.completed_items, str_field(item, &["id"]))
            {
                return Vec::new();
            }
            if matches!(item_type(item), "userMessage" | "user_message") {
                return if phase == Phase::Completed {
                    user_message_text(item)
                        .map(|text| {
                            self.settled = false;
                            AgentEvent::UserMessage { text }
                        })
                        .into_iter()
                        .collect()
                } else {
                    Vec::new()
                };
            }
            if self.settled {
                return Vec::new();
            }
            if matches!(item_type(item), "agentMessage" | "agent_message") {
                if phase == Phase::Started {
                    return Vec::new();
                }
                let mut events = Vec::new();
                if !self.streamed_text.remove(&str_field(item, &["id"])) {
                    let text = str_field(item, &["text"]);
                    if !text.is_empty() {
                        events.push(AgentEvent::TextDelta { text });
                    }
                }
                events.push(AgentEvent::TextDelta {
                    text: "\n\n".into(),
                });
                return events;
            }
            return map_item(phase, item);
        }
        if self.settled {
            return Vec::new();
        }
        match method {
            "thread/tokenUsage/updated" => usage_event(params).into_iter().collect(),
            "item/agentMessage/delta" => {
                let id = item_id(params);
                if !id.is_empty() && self.completed_items.contains(&id) {
                    return Vec::new();
                }
                self.streamed_text.insert(id);
                delta_text(params)
                    .map(|text| AgentEvent::TextDelta { text })
                    .into_iter()
                    .collect()
            }
            "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded" => self.reasoning.map(method, params),
            "turn/completed" | "turn/failed" | "turn/aborted" | "thread/closed" => {
                if method != "thread/closed"
                    && !remember(&mut self.completed_turns, turn_id(params))
                {
                    return Vec::new();
                }
                self.settled = true;
                self.streamed_text.clear();
                let error = turn_error_message(params);
                let status = if method == "turn/failed"
                    || error.is_some()
                    || params.pointer("/turn/status").and_then(Value::as_str) == Some("failed")
                {
                    DoneStatus::Errored
                } else if method == "turn/aborted"
                    || params.pointer("/turn/status").and_then(Value::as_str) == Some("interrupted")
                {
                    DoneStatus::Interrupted
                } else {
                    DoneStatus::Completed
                };
                vec![AgentEvent::Done {
                    status,
                    result: None,
                    error,
                    session_id: Some(child.to_owned()),
                }]
            }
            "error" => vec![AgentEvent::Error {
                message: params
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| params.get("message").and_then(Value::as_str))
                    .unwrap_or("Codex subagent error")
                    .to_owned(),
            }],
            _ => Vec::new(),
        }
    }
}

/// The thread a notification is addressed to: `thread/started` carries it at
/// `params.thread.id`, everything else at `params.threadId`. `None` for
/// thread-less methods (old builds, account noise).
pub(crate) fn notification_thread_id(method: &str, params: &Value) -> Option<String> {
    if method == "thread/started" {
        return params
            .get("thread")
            .and_then(|t| t.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    field(params, &["threadId", "thread_id"])
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// How a notification addressed to a REGISTERED child thread is handled.
///
/// Exported and pure so the table can be asserted directly. The shape (and
/// the fail-open default) is load-bearing: two shipped bugs in t3code's codex
/// runtime came from a catch-all that swallowed everything a child emitted —
/// a child's `error` vanished (the agent card stayed running forever) and a
/// swallowed `serverRequest/resolved` left the parent's approvals stuck.
///
/// - `Subagent`: content/lifecycle attributed to the child (item lifecycles,
///   errors, closure) — mapped to tagged [`AgentEvent::Subagent`] events.
/// - `Consumed`: child bookkeeping with no parent or subagent-doc meaning,
///   plus child thread-lifecycle methods that would rewrite PARENT state if
///   let through (`thread/started` repeats, status/name/usage updates).
/// - `Parent`: pass through to the parent path — unknown methods land here BY
///   DESIGN, so a codex update that adds a notification degrades to "the
///   parent sees it", never to silent loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildRoute {
    Subagent,
    Consumed,
    Parent,
}

pub(crate) fn route_child_notification(method: &str) -> ChildRoute {
    match method {
        // Child message/reasoning deltas DO stream on this wire tagged with
        // the child's threadId (live-verified against codex-cli 0.146.1
        // multi-agent capture) — the subagent transcript is token-level live
        // without touching the rollout file. Child TURN ends are the
        // subagent's terminal signal: `thread/closed` fires only via the
        // collab close_agent tool, which real fan-outs never call
        // (live-verified — chips stayed "running" forever without this).
        "item/started"
        | "item/completed"
        | "item/agentMessage/delta"
        | "item/reasoning/textDelta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "turn/started"
        | "turn/completed"
        | "turn/failed"
        | "turn/aborted"
        | "thread/tokenUsage/updated"
        | "thread/status/changed"
        | "error"
        | "thread/closed" => ChildRoute::Subagent,
        // Child status bookkeeping with no subagent meaning: consumed
        // so it can never settle the PARENT turn (the exact bug class the
        // explicit table exists for).
        // Child chatter with no consumer on this wire.
        "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/fileChange/patchUpdated"
        | "item/plan/delta"
        | "turn/plan/updated"
        | "turn/diff/updated"
        | "thread/name/updated"
        | "thread/settings/updated"
        | "rawResponseItem/completed"
        // Child-owned thread lifecycle that maps onto PARENT state in a
        // naive passthrough (archived/compacted), plus repeat thread/started.
        | "thread/archived"
        | "thread/unarchived"
        | "thread/compacted"
        | "thread/started" => ChildRoute::Consumed,
        // Unknown or parent-owned (approvals bookkeeping, account noise).
        _ => ChildRoute::Parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_parts_preserve_chunking_and_existing_paragraph_breaks() {
        let mut stream = ReasoningStream::default();
        let mut text = String::new();
        for params in [
            json!({"itemId":"r1", "contentIndex":0, "textDelta":"**First"}),
            json!({"itemId":"r1", "contentIndex":0, "textDelta":" heading**\n"}),
            json!({"itemId":"r1", "contentIndex":1, "delta":"\nSecond paragraph.\n\n"}),
            // An empty delta must not consume the next item's boundary.
            json!({"itemId":"r2", "contentIndex":0, "delta":""}),
            json!({"item_id":"r2", "content_index":0, "delta":"Third paragraph."}),
        ] {
            for event in stream.map("item/reasoning/textDelta", &params) {
                if let AgentEvent::ReasoningDelta { text: delta } = event {
                    text.push_str(&delta);
                }
            }
        }
        assert_eq!(
            text,
            "**First heading**\n\nSecond paragraph.\n\nThird paragraph."
        );
    }

    #[test]
    fn user_message_text_accepts_both_shapes() {
        assert_eq!(
            user_message_text(&json!({"text": "steer"})),
            Some("steer".into())
        );
        assert_eq!(
            user_message_text(&json!({"content": [
                {"type": "text", "text": "a"},
                {"type": "text", "text": "b"},
            ]})),
            Some("a\n\nb".into())
        );
        assert_eq!(user_message_text(&json!({"text": "  "})), None);
        assert_eq!(user_message_text(&json!({})), None);
    }

    #[test]
    fn delta_accepts_both_spellings() {
        assert_eq!(delta_text(&json!({"delta": "a"})), Some("a".into()));
        assert_eq!(delta_text(&json!({"textDelta": "b"})), Some("b".into()));
        assert_eq!(delta_text(&json!({"delta": ""})), None);
        assert_eq!(delta_text(&json!({})), None);
    }

    #[test]
    fn command_execution_maps_exit_code_to_error() {
        let started = map_item(
            Phase::Started,
            &json!({"type": "commandExecution", "id": "c1", "command": "ls"}),
        );
        assert_eq!(
            started,
            vec![AgentEvent::ToolCall {
                id: "c1".into(),
                call: ToolCall::Exec {
                    command: "ls".into()
                },
            }]
        );
        let completed = map_item(
            Phase::Completed,
            &json!({"type": "command_execution", "id": "c1", "status": "completed", "exit_code": 2}),
        );
        assert_eq!(
            completed,
            vec![AgentEvent::ToolResult {
                id: "c1".into(),
                is_error: true,
                output: None,
                diff: None,
            }]
        );
    }

    #[test]
    fn file_change_variants_map_to_typed_calls() {
        let add = map_item(
            Phase::Started,
            &json!({"type": "fileChange", "id": "f1", "changes": [{"path": "/a.rs", "kind": "add"}]}),
        );
        assert_eq!(
            add,
            vec![AgentEvent::ToolCall {
                id: "f1".into(),
                call: ToolCall::WriteFile {
                    path: "/a.rs".into(),
                    content: None
                },
            }]
        );
        let update = map_item(
            Phase::Completed,
            &json!({"type": "fileChange", "id": "f2", "status": "declined",
                    "changes": [{"path": "/b.rs", "kind": "update"}]}),
        );
        assert_eq!(
            update,
            vec![
                AgentEvent::ToolCall {
                    id: "f2".into(),
                    call: ToolCall::EditFile {
                        path: "/b.rs".into(),
                        old_string: None,
                        new_string: None
                    },
                },
                AgentEvent::ToolResult {
                    id: "f2".into(),
                    is_error: true,
                    output: None,
                    diff: None,
                },
            ]
        );
        let multi = map_item(
            Phase::Started,
            &json!({"type": "fileChange", "id": "f3",
                    "changes": [{"path": "/a"}, {"path": "/b", "kind": "delete"}]}),
        );
        assert_eq!(
            multi,
            vec![AgentEvent::ToolCall {
                id: "f3".into(),
                call: ToolCall::ApplyPatch { path: None },
            }]
        );
    }

    #[test]
    fn usage_reads_last_snapshot_under_both_spellings() {
        assert_eq!(
            usage_event(&json!({"tokenUsage": {"last": {"inputTokens": 42, "outputTokens": 7}}})),
            Some(AgentEvent::Usage {
                input_tokens: 42,
                output_tokens: 7
            })
        );
        assert_eq!(
            usage_event(&json!({"token_usage": {"last": {"input_tokens": 1, "output_tokens": 2}}})),
            Some(AgentEvent::Usage {
                input_tokens: 1,
                output_tokens: 2
            })
        );
        assert_eq!(usage_event(&json!({})), None);
    }

    #[test]
    fn v1_spawns_and_controls_have_distinct_roles() {
        for tool in ["spawnAgent", "spawn_agent"] {
            let mut item = json!({"type":"collabAgentToolCall", "id":"spawn", "tool":tool,
                "status":"completed", "receiverThreadIds":["child"], "model":"child-model"});
            assert_eq!(collab_spawn_child(&item), Some("child"));
            let events = map_item(Phase::Completed, &item);
            assert!(matches!(&events[0], AgentEvent::ToolCall { call, .. }
                if call.is_subagent_spawn() && call.subagent_model() == Some("child-model")));
            assert!(matches!(
                &events[1],
                AgentEvent::ToolResult {
                    is_error: false,
                    ..
                }
            ));
            item["status"] = "failed".into();
            assert_eq!(collab_spawn_child(&item), None);
            assert!(matches!(
                map_item(Phase::Completed, &item).last(),
                Some(AgentEvent::ToolResult { is_error: true, .. })
            ));
        }
        for tool in [
            "sendInput",
            "send_input",
            "wait",
            "closeAgent",
            "resumeAgent",
            "futureControl",
        ] {
            let item = json!({"type":"collabAgentToolCall", "id":"control", "tool":tool,
                "receiverThreadIds":["child"]});
            assert_eq!(collab_spawn_child(&item), None);
            assert!(
                matches!(&map_item(Phase::Started, &item)[0], AgentEvent::ToolCall { call, .. } if !call.is_subagent_spawn())
            );
        }
        for receivers in [json!([]), json!(["one", "two"]), json!([""])] {
            assert_eq!(
                collab_spawn_child(
                    &json!({"type":"collabAgentToolCall", "tool":"spawnAgent", "receiverThreadIds":receivers})
                ),
                None
            );
        }
    }

    #[test]
    fn activity_updates_are_not_spawns_in_either_item_phase() {
        for kind in [
            "interacted",
            "completed",
            "failed",
            "errored",
            "futureActivity",
        ] {
            let item = json!({"type":"subAgentActivity", "id":"activity", "kind":kind,
                "agentThreadId":"child", "agentPath":"/root/alpha"});
            assert!(map_item(Phase::Started, &item).is_empty());
            assert!(map_item(Phase::Completed, &item).is_empty());
        }
    }

    #[test]
    fn sub_agent_activity_maps_to_a_named_parent_chip() {
        let started = map_item(
            Phase::Started,
            &json!({"type": "subAgentActivity", "id": "call_1", "kind": "started",
                    "agentThreadId": "child-1", "agentPath": "/root/alpha"}),
        );
        assert_eq!(started.len(), 1);
        assert!(matches!(
            &started[0],
            AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
                if id == "call_1" && name == "Agent: alpha"
        ));
        let completed = map_item(
            Phase::Completed,
            &json!({"type": "subAgentActivity", "id": "call_1", "kind": "started",
                    "agentThreadId": "child-1", "agentPath": "/root/alpha"}),
        );
        assert!(matches!(
            completed.last(),
            Some(AgentEvent::ToolResult { id, is_error: false, .. }) if id == "call_1"
        ));
    }

    #[test]
    fn child_usage_notification_keeps_only_the_provider_reported_snapshot() {
        let mut child = ChildStream::default();
        assert_eq!(
            child.map(
                "child-1",
                "thread/tokenUsage/updated",
                &json!({
                    "threadId": "child-1",
                    "tokenUsage": { "last": { "inputTokens": 21, "outputTokens": 3 } }
                })
            ),
            vec![AgentEvent::Usage {
                input_tokens: 21,
                output_tokens: 3
            }]
        );
    }

    #[test]
    fn child_idle_and_active_are_observed_without_settling_a_turn() {
        let mut child = ChildStream::default();
        for (native, expected) in [
            ("idle", luna_proto::ChildRuntimeStatus::Idle),
            ("active", luna_proto::ChildRuntimeStatus::Active),
        ] {
            assert_eq!(
                child.map(
                    "child-1",
                    "thread/status/changed",
                    &json!({
                        "threadId": "child-1", "status": { "type": native }
                    })
                ),
                vec![AgentEvent::ChildStatus { status: expected }]
            );
        }
    }

    #[test]
    fn child_routing_table_fails_open_to_parent() {
        // Child content/lifecycle → the subagent path, deltas included
        // (live-verified: child threads stream them on this wire).
        for m in [
            "item/started",
            "item/completed",
            "item/agentMessage/delta",
            "item/reasoning/textDelta",
            "error",
            "thread/closed",
            "thread/tokenUsage/updated",
            "thread/status/changed",
        ] {
            assert_eq!(route_child_notification(m), ChildRoute::Subagent, "{m}");
        }
        // Child TURN ENDS are the subagent's terminal signal…
        for m in ["turn/completed", "turn/aborted", "turn/failed"] {
            assert_eq!(route_child_notification(m), ChildRoute::Subagent, "{m}");
        }
        // A child turn start can reopen a completed assignment. It must never
        // reach the parent turn router.
        assert_eq!(
            route_child_notification("turn/started"),
            ChildRoute::Subagent
        );
        // Child-owned thread lifecycle would rewrite parent state — consumed.
        for m in ["thread/archived", "thread/compacted", "thread/started"] {
            assert_eq!(route_child_notification(m), ChildRoute::Consumed, "{m}");
        }
        // Unknown methods degrade to "parent sees it", never silent loss
        // (the two-shipped-bugs rule).
        for m in [
            "serverRequest/resolved",
            "thread/somethingBrandNew",
            "account/rateLimits/updated",
        ] {
            assert_eq!(route_child_notification(m), ChildRoute::Parent, "{m}");
        }
    }

    #[test]
    fn notification_thread_ids_read_both_shapes() {
        assert_eq!(
            notification_thread_id("thread/started", &json!({"thread": {"id": "th-c"}})),
            Some("th-c".into())
        );
        assert_eq!(
            notification_thread_id(
                "turn/completed",
                &json!({"threadId": "th-1", "turn": {"id": "t"}})
            ),
            Some("th-1".into())
        );
        assert_eq!(
            notification_thread_id("error", &json!({"message": "x"})),
            None
        );
    }

    #[test]
    fn turn_error_extraction() {
        assert_eq!(
            turn_error_message(&json!({"turn": {"id": "t", "error": {"message": "boom"}}})),
            Some("boom".into())
        );
        assert_eq!(turn_error_message(&json!({"turn": {"id": "t"}})), None);
        assert_eq!(turn_error_message(&json!({"turn": {"error": null}})), None);
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn context_uses_latest_call_not_thread_total() {
        assert_eq!(
            context_usage_event(&json!({"tokenUsage": {
                "last": {"totalTokens": 42000}, "total": {"totalTokens": 900000}, "modelContextWindow": 200000
            }})),
            Some(AgentEvent::ContextUsage {
                tokens: Some(42000),
                window: Some(200000)
            })
        );
        assert_eq!(
            context_usage_event(&json!({"token_usage": {
                "last": {"input_tokens": 0, "output_tokens": 0}, "model_context_window": 0
            }})),
            Some(AgentEvent::ContextUsage {
                tokens: Some(0),
                window: None
            })
        );
        assert_eq!(
            context_usage_event(&json!({"tokenUsage": {"total": {"totalTokens": 100}}})),
            None
        );
    }

    #[test]
    fn context_compaction_requires_root_thread_turn_and_item_correlation() {
        let mut stream = ContextCompactionStream::default();
        let started = json!({
            "threadId": "root-thread",
            "turnId": "turn-1",
            "item": {"id": "compact-1", "type": "contextCompaction"}
        });
        let item = &started["item"];
        assert!(
            stream
                .map_item(
                    Phase::Started,
                    "root-thread",
                    Some("other-turn"),
                    &started,
                    item,
                )
                .is_none()
        );
        let running = stream
            .map_item(
                Phase::Started,
                "root-thread",
                Some("turn-1"),
                &started,
                item,
            )
            .unwrap();
        assert!(matches!(
            running,
            AgentEvent::ContextCompaction {
                status: ContextOperationStatus::Running,
                ref operation_id,
                ref thread_id,
                ref turn_id,
            } if operation_id == "codex-auto:root-thread:turn-1:compact-1"
                && thread_id == "root-thread"
                && turn_id == "turn-1"
        ));

        let stale = json!({
            "threadId": "root-thread",
            "turnId": "turn-1",
            "item": {"id": "compact-stale", "type": "contextCompaction"}
        });
        assert!(
            stream
                .map_item(
                    Phase::Completed,
                    "root-thread",
                    Some("turn-1"),
                    &stale,
                    &stale["item"],
                )
                .is_none()
        );
        let child = json!({
            "threadId": "child-thread",
            "turnId": "turn-1",
            "item": {"id": "compact-1", "type": "contextCompaction"}
        });
        assert!(
            stream
                .map_item(
                    Phase::Completed,
                    "root-thread",
                    Some("turn-1"),
                    &child,
                    &child["item"],
                )
                .is_none()
        );
        let completed = stream
            .map_item(
                Phase::Completed,
                "root-thread",
                Some("turn-1"),
                &started,
                item,
            )
            .unwrap();
        assert!(matches!(
            completed,
            AgentEvent::ContextCompaction {
                status: ContextOperationStatus::Completed,
                ..
            }
        ));
        assert!(stream.take_unknown().is_none());
    }

    #[test]
    fn interrupted_context_compaction_becomes_unknown_once() {
        let mut stream = ContextCompactionStream::default();
        let started = json!({
            "threadId": "root-thread",
            "turnId": "turn-1",
            "item": {"id": "compact-1", "type": "contextCompaction"}
        });
        stream
            .map_item(
                Phase::Started,
                "root-thread",
                Some("turn-1"),
                &started,
                &started["item"],
            )
            .unwrap();
        assert!(matches!(
            stream.unknown_for_turn("root-thread", "turn-1"),
            Some(AgentEvent::ContextCompaction {
                status: ContextOperationStatus::Unknown,
                ..
            })
        ));
        assert!(stream.unknown_for_turn("root-thread", "turn-1").is_none());
    }
}

#[cfg(test)]
mod generated_image_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_generation_lifecycle_ignores_inline_result_and_preserves_ids() {
        for (kind, path_key) in [
            ("imageGeneration", "savedPath"),
            ("image_generation", "saved_path"),
        ] {
            let mut item = json!({"id":"img-1", "type":kind, "status":"completed", "result":"INLINE_IMAGE_SENTINEL".repeat(200_000), "revisedPrompt":"private prompt", "failure":null});
            item[path_key] = json!("/codex/generated_images/picture.png");
            let started = map_item(Phase::Started, &item);
            assert_eq!(
                started,
                vec![AgentEvent::ToolCall {
                    id: "img-1".into(),
                    call: ToolCall::Unknown {
                        name: "Generate image".into(),
                        input: None
                    }
                }]
            );
            let completed = map_item(Phase::Completed, &item);
            assert_eq!(completed.len(), 3);
            assert_eq!(completed[0], started[0]);
            assert!(
                matches!(&completed[1], AgentEvent::ToolResult { id, is_error: false, output: None, .. } if id == "img-1")
            );
            assert!(
                matches!(&completed[2], AgentEvent::GeneratedImage { id, path, name, .. } if id == "img-1:image" && path == "/codex/generated_images/picture.png" && name == "picture.png")
            );
            assert_eq!(map_item(Phase::Completed, &item), completed);
            let wire = serde_json::to_string(&completed).unwrap();
            assert!(wire.len() < 500);
            assert!(!wire.contains("INLINE_IMAGE_SENTINEL"));
            assert!(!wire.contains("private prompt"));
        }
    }

    #[test]
    fn image_generation_errors_resolve_the_chip_without_an_image() {
        for (extra, expected) in [
            (
                json!({"failure":{"type":"usageLimitExceeded"}, "savedPath":"/must/not/use.png"}),
                "Image generation usage limit exceeded",
            ),
            (
                json!({"failure":{"type":"futureFailure","message":"untrusted payload"}}),
                "Image generation failed",
            ),
            (
                json!({"status":"failed", "savedPath":"/must/not/use.png"}),
                "Image generation failed",
            ),
            (
                json!({"savedPath":null}),
                "Image generation completed without a saved file",
            ),
            (
                json!({"savedPath":"  "}),
                "Image generation completed without a saved file",
            ),
            (json!({}), "Image generation completed without a saved file"),
        ] {
            let mut item = json!({"type":"imageGeneration", "id":"i", "status":"completed", "result":"INLINE_SENTINEL"});
            item.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let events = map_item(Phase::Completed, &item);
            assert_eq!(events.len(), 3);
            assert!(matches!(events[0], AgentEvent::ToolCall { .. }));
            assert!(matches!(
                events[1],
                AgentEvent::ToolResult { is_error: true, .. }
            ));
            assert_eq!(
                events[2],
                AgentEvent::Error {
                    message: expected.into()
                }
            );
        }
    }
}
