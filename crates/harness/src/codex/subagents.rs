//! Stable ownership of child traffic. A thread belongs to its original spawn
//! call; activity ids and early thread notifications are never document ids.

use std::collections::{HashMap, HashSet};

use luna_proto::AgentEvent;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::normalize::{ChildStream, Phase, collab_spawn_child, item_type, map_item};

const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Pending {
    events: Vec<AgentEvent>,
    bytes: usize,
}

pub(super) struct Subagents {
    root: String,
    spawns: HashMap<String, String>,
    pending: HashMap<String, Pending>,
    pending_bytes: usize,
    warned_overflow: bool,
    streams: HashMap<String, ChildStream>,
    emitted_spawns: HashSet<String>,
    resolved_spawns: HashSet<String>,
    /// Child thread → its live turn, observed from `turn/started` on that
    /// thread. Server requests from a child are honoured only for this turn.
    live_turns: HashMap<String, (String, CancellationToken)>,
    /// Child thread → display label (agentPath leaf).
    labels: HashMap<String, String>,
}

impl Subagents {
    pub(super) fn new(root: String) -> Self {
        Self {
            root,
            spawns: HashMap::new(),
            pending: HashMap::new(),
            pending_bytes: 0,
            warned_overflow: false,
            streams: HashMap::new(),
            emitted_spawns: HashSet::new(),
            resolved_spawns: HashSet::new(),
            live_turns: HashMap::new(),
            labels: HashMap::new(),
        }
    }

    /// thread/resume returns the stored parent items. Rebuild ownership without
    /// replaying chips or content that already lives in Luna's documents.
    pub(super) fn restore(&mut self, thread: &Value) {
        self.restore_turns(
            thread
                .get("turns")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
    }

    /// Same ownership rebuild from paginated `thread/turns/list` pages.
    pub(super) fn restore_turns(&mut self, turns: &[Value]) {
        for item in turns.iter().flat_map(|turn| {
            turn.get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        }) {
            if super::normalize::is_collab_spawn(item)
                || matches!(item_type(item), "subAgentActivity" | "sub_agent_activity")
            {
                self.parent_item(Phase::Completed, item);
            }
        }
    }

    pub(super) fn notification(
        &mut self,
        child: &str,
        method: &str,
        params: &Value,
    ) -> Vec<AgentEvent> {
        self.track_turn(child, method, params);
        let events = self
            .streams
            .entry(child.to_owned())
            .or_default()
            .map(child, method, params);
        self.route(child, events)
    }

    /// The routing for a server request addressed to `child`: only a child
    /// whose turn this connection saw start, and only while it is live.
    pub(super) fn request_route(&self, child: &str) -> Option<super::approvals::SubagentRequest> {
        if child.is_empty() || child == self.root {
            return None;
        }
        let (turn_id, cancel) = self.live_turns.get(child)?;
        Some(super::approvals::SubagentRequest {
            thread_id: child.to_owned(),
            turn_id: turn_id.clone(),
            label: self.label(child),
            cancel: cancel.clone(),
        })
    }

    /// A child thread this connection has seen (for `serverRequest/resolved`).
    pub(super) fn knows(&self, child: &str) -> bool {
        child != self.root
            && (self.spawns.contains_key(child)
                || self.streams.contains_key(child)
                || self.live_turns.contains_key(child))
    }

    fn label(&self, child: &str) -> String {
        self.labels.get(child).cloned().unwrap_or_else(|| {
            let short: String = child.chars().take(8).collect();
            format!("agent {short}")
        })
    }

    fn track_turn(&mut self, child: &str, method: &str, params: &Value) {
        if child.is_empty() || child == self.root {
            return;
        }
        let turn = params
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .unwrap_or("");
        match method {
            "turn/started" if !turn.is_empty() => {
                if let Some((previous, cancel)) = self.live_turns.get(child) {
                    if previous == turn {
                        return;
                    }
                    cancel.cancel();
                }
                self.live_turns.insert(
                    child.to_owned(),
                    (turn.to_owned(), CancellationToken::new()),
                );
            }
            "turn/completed" => {
                if self
                    .live_turns
                    .get(child)
                    .is_some_and(|(live, _)| live == turn)
                    && let Some((_, cancel)) = self.live_turns.remove(child)
                {
                    cancel.cancel();
                }
            }
            "thread/closed" => {
                if let Some((_, cancel)) = self.live_turns.remove(child) {
                    cancel.cancel();
                }
            }
            _ => {}
        }
    }

    /// Session teardown: every parked child request closes.
    pub(super) fn close_all(&mut self) {
        for (_, (_, cancel)) in self.live_turns.drain() {
            cancel.cancel();
        }
    }

    pub(super) fn parent_item(&mut self, phase: Phase, item: &Value) -> Vec<AgentEvent> {
        let activity = matches!(item_type(item), "subAgentActivity" | "sub_agent_activity");
        let child = if activity {
            let path = item.get("agentPath").and_then(Value::as_str).unwrap_or("");
            if matches!(path, "/" | "/root") {
                return Vec::new();
            }
            if let Some(thread) = item.get("agentThreadId").and_then(Value::as_str)
                && let Some(leaf) = path.rsplit('/').find(|leaf| !leaf.is_empty())
                && thread != self.root
            {
                self.labels
                    .entry(thread.to_owned())
                    .or_insert_with(|| leaf.to_owned());
            }
            if !matches!(
                item.get("kind").and_then(Value::as_str),
                Some("started" | "spawned")
            ) {
                return Vec::new();
            }
            item.get("agentThreadId").and_then(Value::as_str)
        } else {
            collab_spawn_child(item)
        };
        if child == Some(self.root.as_str()) {
            return Vec::new();
        }
        let mut item = std::borrow::Cow::Borrowed(item);
        let mut buffered = Vec::new();
        if let Some(child) = child {
            let call = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            buffered = self.bind(child, &call);
            if let Some(owner) = self.spawns.get(child) {
                item.to_mut()["id"] = owner.clone().into();
            }
        }
        let mut events: Vec<_> = map_item(phase, &item)
            .into_iter()
            .filter(|event| match event {
                // Completed items refresh tool metadata, but a spawn chip must
                // appear once even when its parent text segment has already ended.
                AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => {
                    self.emitted_spawns.insert(id.clone())
                }
                AgentEvent::ToolResult { id, .. } if self.emitted_spawns.contains(id) => {
                    self.resolved_spawns.insert(id.clone())
                }
                _ => true,
            })
            .collect();
        events.extend(buffered);
        events
    }

    /// Call only for a spawn. Emit its parent chip BEFORE the returned events,
    /// so even a child that finished before registration binds to a real chip.
    pub(super) fn bind(&mut self, child: &str, spawn: &str) -> Vec<AgentEvent> {
        if child.is_empty() || child == self.root || spawn.is_empty() {
            return Vec::new();
        }
        let owner = self
            .spawns
            .entry(child.to_owned())
            .or_insert_with(|| spawn.to_owned());
        let Some(pending) = self.pending.remove(child) else {
            return Vec::new();
        };
        self.pending_bytes -= pending.bytes;
        pending.events.into_iter().map(|e| tag(owner, e)).collect()
    }

    pub(super) fn route(&mut self, child: &str, events: Vec<AgentEvent>) -> Vec<AgentEvent> {
        if child.is_empty() || child == self.root {
            return Vec::new();
        }
        if let Some(owner) = self.spawns.get(child) {
            return events.into_iter().map(|e| tag(owner, e)).collect();
        }
        // A child's output may beat both thread/started and the spawn result.
        // Bound the aggregate backlog for threads that never acquire a spawn.
        for event in events {
            let bytes = serde_json::to_vec(&event).map_or(MAX_PENDING_BYTES, |v| v.len());
            if self.pending_bytes.saturating_add(bytes) > MAX_PENDING_BYTES {
                if !self.warned_overflow {
                    tracing::warn!("Codex unbound child backlog full; dropping excess events");
                    self.warned_overflow = true;
                }
                continue;
            }
            self.pending_bytes += bytes;
            let pending = self.pending.entry(child.to_owned()).or_default();
            pending.bytes += bytes;
            pending.events.push(event);
        }
        Vec::new()
    }
}

fn tag(spawn: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Subagent {
        parent_tool_use_id: spawn.to_owned(),
        event: Box::new(event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: text.into() }
    }

    #[test]
    fn binding_is_stable_and_children_are_independent() {
        let mut children = Subagents::new("root".into());
        children.bind("alpha", "spawn-alpha");
        children.bind("beta", "spawn-beta");
        children.bind("alpha", "later-activity");
        assert_eq!(
            children.route("alpha", vec![text("a")]),
            vec![tag("spawn-alpha", text("a"))]
        );
        assert_eq!(
            children.route("beta", vec![text("b")]),
            vec![tag("spawn-beta", text("b"))]
        );
    }

    #[test]
    fn early_content_waits_for_the_spawn_and_drains_once_in_order() {
        let mut children = Subagents::new("root".into());
        assert!(
            children
                .route("alpha", vec![text("first"), text("second")])
                .is_empty()
        );
        assert_eq!(
            children.bind("alpha", "spawn"),
            vec![tag("spawn", text("first")), tag("spawn", text("second"))]
        );
        assert!(children.bind("alpha", "spawn").is_empty());
        assert_eq!(children.pending_bytes, 0);
    }

    #[test]
    fn root_and_missing_ids_never_bind_or_buffer() {
        let mut children = Subagents::new("root".into());
        for child in ["root", ""] {
            assert!(children.bind(child, "spawn").is_empty());
            assert!(children.route(child, vec![text("noise")]).is_empty());
        }
        children.bind("alpha", "");
        assert!(children.spawns.is_empty());
        assert!(children.pending.is_empty());
    }

    #[test]
    fn unbound_backlog_is_bounded_without_affecting_known_children() {
        let mut children = Subagents::new("root".into());
        children.route("unknown", vec![text(&"x".repeat(MAX_PENDING_BYTES))]);
        assert!(children.pending.is_empty());
        children.bind("alpha", "spawn");
        assert_eq!(
            children.route("alpha", vec![text("ok")]),
            vec![tag("spawn", text("ok"))]
        );
    }
}
