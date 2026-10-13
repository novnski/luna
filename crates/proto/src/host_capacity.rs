//! AW-10 host turn capacity: how many turns this execution host runs against
//! its limit, and which new turns wait for a free slot and why. Read with
//! `GetHostCapacity`, changed with `SetHostCapacity`; both stay on the device
//! that runs the engine.

use serde::{Deserialize, Serialize};

use crate::SessionStatus;

/// The largest limit a host accepts.
pub const HOST_TURN_LIMIT_MAX: u32 = 32;

/// What a waiting start would begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostCapacityWaitKind {
    /// A new message (or automation, Board or review run) for an idle task.
    /// `source_id` is its command id; `cancelCapacityWait` withdraws it.
    Send,
    /// The next queued follow-up of an idle task. `source_id` is the queue
    /// row id; the row stays held in its task's queue.
    QueuedMessage,
    /// A scheduled automatic retry. `source_id` is the recovery chain id.
    AutomaticRetry,
}

/// One start waiting for a free turn slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapacityWait {
    pub task_id: String,
    pub kind: HostCapacityWaitKind,
    pub source_id: String,
    /// 1-based place in line.
    pub position: u32,
    /// When the waiting work was issued (epoch ms).
    pub since_ms: i64,
}

/// One task whose turn holds a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapacityTurn {
    pub task_id: String,
    /// Working, or parked on an approval or question (which still holds its
    /// provider).
    pub status: SessionStatus,
}

/// `GetHostCapacity` / `SetHostCapacity`: this host's turns against its
/// limit. Lowering the limit never stops a running turn, so `active` may
/// exceed `limit`; explicit Send now and delegated child chats are counted
/// but never held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapacity {
    pub device_id: String,
    /// Most turns started at once; `None` when the host has no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Turns running or parked on a question, by task.
    #[serde(default)]
    pub active: Vec<HostCapacityTurn>,
    /// Starts admitted whose turn has not registered yet; they hold slots.
    #[serde(default)]
    pub starting: u32,
    /// Starts waiting for a free slot, in admission order.
    #[serde(default)]
    pub waiting: Vec<HostCapacityWait>,
    pub read_at_ms: i64,
}

impl HostCapacity {
    /// Slots in use: active turns plus admitted starts.
    pub fn occupied(&self) -> u32 {
        (self.active.len() as u32).saturating_add(self.starting)
    }
}

/// `SetHostCapacity`: `None` removes the limit; otherwise 1 to
/// [`HOST_TURN_LIMIT_MAX`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetHostCapacityRequest {
    #[serde(default)]
    pub limit: Option<u32>,
}
