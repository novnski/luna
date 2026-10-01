//! Nested native subagent chips (CC-08).
//!
//! Providers tag a child's traffic with the spawn call that launched it,
//! flat: a grandchild's frames carry the grandchild spawn id. That spawn's
//! chip lives in its PARENT CHILD's transcript, not in the root chat, so
//! decorating only root chips left the grandchild's transcript unreachable
//! (no link, no lifecycle). This index records which child transcript owns
//! each nested spawn chip, learned from the tagged spawn call itself.

use std::collections::HashMap;
use std::sync::Arc;

use luna_doc::SessionDoc;
use luna_proto::AgentEvent;

use super::{Inner, SubagentSink, subagent_doc_id};

/// Bound on remembered nested spawns per run (a spawn chip each).
const MAX_NESTED_SPAWNS: usize = 4096;

#[derive(Default)]
pub(super) struct NestedSpawns {
    /// Nested spawn tool_use id → the spawn id whose transcript holds it.
    owners: HashMap<String, String>,
}

impl NestedSpawns {
    /// Learn ownership from a tagged event: a SPAWN call inside child
    /// `owner`'s transcript makes that call's chip `owner`'s. Genus-gated:
    /// an ordinary tool call never creates a link.
    pub(super) fn observe(&mut self, owner: &str, event: &AgentEvent) {
        if let AgentEvent::ToolCall { id, call } = event
            && call.is_subagent_spawn()
            && !id.is_empty()
            && id != owner
            && (self.owners.len() < MAX_NESTED_SPAWNS || self.owners.contains_key(id))
        {
            self.owners
                .entry(id.clone())
                .or_insert_with(|| owner.to_owned());
        }
    }

    /// The transcript holding a nested spawn's chip (its owning child's), or
    /// `None` for a root-level spawn. A live owner sink is flushed first, so
    /// a chip it folded but has not written yet exists when it is updated;
    /// its folded copy never carries subagent fields, so later flushes keep
    /// what is written here.
    pub(super) fn chip_doc(
        &self,
        spawn: &str,
        inner: &Inner,
        chat_id: &str,
        sinks: &mut HashMap<String, SubagentSink>,
        device_id: &str,
    ) -> Option<Arc<SessionDoc>> {
        let owner = self.owners.get(spawn)?;
        if let Some(sink) = sinks.get_mut(owner) {
            sink.flush(device_id);
            return Some(sink.doc.clone());
        }
        // A settled owner's transcript is closed but still the chip's home.
        match inner.doc_host()?.open(&subagent_doc_id(chat_id, owner)) {
            Ok(handle) => Some(handle.doc_arc()),
            Err(error) => {
                tracing::warn!(chat = %chat_id, %owner, %error, "nested spawn owner doc open failed");
                None
            }
        }
    }
}
