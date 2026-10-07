//! Native subagent spawns: chip registration, child binding, replay and
//! completion.

use super::*;

/// A spawned subagent (a `task` tool call on OUR session).
pub(super) struct ChildRun {
    /// The parent-feed tool part id its traffic is tagged with.
    pub(super) parent_tool_use_id: String,
    pub(super) feed: SessionFeed,
    /// Chip settled (tagged Done sent); late traffic drops.
    pub(super) done: bool,
}

/// A `task` chip awaiting its child session id.
pub(super) struct PendingSpawn {
    pub(super) tool_part_id: String,
    pub(super) description: String,
}

/// A completed/errored `task` part → (child session id, failed).
pub(super) fn task_completion(part: &Value) -> Option<(String, bool)> {
    if part.get("type").and_then(Value::as_str) != Some("tool")
        || part.get("tool").and_then(Value::as_str) != Some("task")
    {
        return None;
    }
    let state = part.get("state")?;
    let status = state.get("status").and_then(Value::as_str)?;
    if !matches!(status, "completed" | "error") {
        return None;
    }
    let child = state
        .get("metadata")
        .and_then(|m| m.get("sessionId").or_else(|| m.get("sessionID")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some((child.to_owned(), status == "error"))
}

/// Bind a fresh child session to a spawn chip: description match against the
/// child title (`"{description} (@{agent} subagent)"`), else FIFO.
pub(super) fn bind_child(
    children: &mut HashMap<String, ChildRun>,
    pending: &mut VecDeque<PendingSpawn>,
    child_id: &str,
    title: &str,
) -> bool {
    let ix = pending
        .iter()
        .position(|p| !p.description.is_empty() && title.starts_with(&p.description))
        .or(if pending.is_empty() { None } else { Some(0) });
    match ix.and_then(|i| pending.remove(i)) {
        Some(p) => {
            children.insert(
                child_id.to_owned(),
                ChildRun {
                    parent_tool_use_id: p.tool_part_id,
                    feed: SessionFeed::default(),
                    done: false,
                },
            );
            true
        }
        None => false,
    }
}

/// Replay parts that raced ahead of their message's role fact.
pub(super) fn replay_pending(
    feed: &mut SessionFeed,
    message: &str,
    is_main: bool,
    turn: &mut TurnState,
) -> Vec<AgentEvent> {
    let held: Vec<Value> = std::mem::take(&mut feed.pending_parts)
        .into_iter()
        .filter(|part| part.get("messageID").and_then(Value::as_str) == Some(message))
        .collect();
    let events: Vec<AgentEvent> = held
        .iter()
        .flat_map(|part| part_snapshot_events(feed, part, is_main, None))
        .collect();
    if is_main {
        mark_content(turn, &events);
    }
    events
}

/// Spawn-registration context for the MAIN feed (`task` parts register
/// pending chips; child feeds pass `None` — no recursive viz).
pub(super) type SpawnCtx<'a> = (
    &'a mut HashMap<String, ChildRun>,
    &'a mut VecDeque<PendingSpawn>,
    &'a mut HashMap<String, String>,
);

/// Register a `task` spawn chip and bind it eagerly when the tool's own
/// metadata already names the child session (opencode stamps
/// `state.metadata.sessionId` at spawn).
pub(super) fn register_spawn(
    children: &mut HashMap<String, ChildRun>,
    pending: &mut VecDeque<PendingSpawn>,
    unbound: &mut HashMap<String, String>,
    part: &Value,
    part_id: &str,
    input: &Value,
) {
    let known = pending.iter().any(|p| p.tool_part_id == part_id)
        || children.values().any(|c| c.parent_tool_use_id == part_id);
    if known {
        return;
    }
    let child_id = part
        .get("state")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("sessionId").or_else(|| m.get("sessionID")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !child_id.is_empty() && !children.contains_key(child_id) {
        unbound.remove(child_id);
        children.insert(
            child_id.to_owned(),
            ChildRun {
                parent_tool_use_id: part_id.to_owned(),
                feed: SessionFeed::default(),
                done: false,
            },
        );
        return;
    }
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // A child that raced ahead of its chip binds now.
    let matched = unbound
        .iter()
        .find(|(_, title)| !description.is_empty() && title.starts_with(&description))
        .map(|(id, _)| id.clone())
        .or_else(|| {
            (unbound.len() == 1)
                .then(|| unbound.keys().next().cloned())
                .flatten()
        });
    if let Some(id) = matched {
        unbound.remove(&id);
        children.insert(
            id,
            ChildRun {
                parent_tool_use_id: part_id.to_owned(),
                feed: SessionFeed::default(),
                done: false,
            },
        );
        return;
    }
    pending.push_back(PendingSpawn {
        tool_part_id: part_id.to_owned(),
        description,
    });
}
