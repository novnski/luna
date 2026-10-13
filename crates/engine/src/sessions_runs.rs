//! The live-run table: the one owner of "may a run start" (CORE-03).
//!
//! It holds every chat's live run, counts dispatches between admission and
//! run registration, keeps each chat's transition gate, and closes when
//! shutdown begins. A dispatch enters through [`RunTable::enter_dispatch`]:
//! once the table is closed it is refused before it reaches a provider, so
//! no run can register after shutdown's final sweep and outlive the engine.
//!
//! It also owns the host turn line (AW-10, [`TurnCapacity`]): how many turns
//! may be active at once, the starts admitted but not yet running, and the
//! new turns waiting for a free slot, in issue order.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use luna_proto::{HOST_TURN_LIMIT_MAX, HostCapacityWait, HostCapacityWaitKind};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use super::{RunHandle, lock};
use crate::EngineError;

pub(super) struct RunTable {
    /// chat_id → live run.
    runs: Mutex<HashMap<String, RunHandle>>,
    /// Dispatches between admission and run registration. Shutdown waits for
    /// them, so a run registered after its first sweep (for example a queue
    /// drain already in flight) is still interrupted.
    dispatches_in_flight: AtomicUsize,
    /// Set when shutdown begins; never cleared.
    closed: AtomicBool,
    /// Serializes runtime replacement/start with permission-mode mutation for
    /// one chat. Without this, an async idle retirement can cancel a fresh run
    /// that started after the mutation returned.
    transition_gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// AW-10 host turn limit and line.
    capacity: Arc<TurnCapacity>,
}

impl RunTable {
    pub(super) fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            dispatches_in_flight: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            transition_gates: Mutex::new(HashMap::new()),
            capacity: Arc::new(TurnCapacity::default()),
        }
    }

    /// The host turn limit and line (AW-10).
    pub(super) fn capacity(&self) -> &Arc<TurnCapacity> {
        &self.capacity
    }

    /// The live runs, keyed by chat.
    pub(super) fn lock(&self) -> MutexGuard<'_, HashMap<String, RunHandle>> {
        lock(&self.runs)
    }

    /// Counts one dispatch until the guard drops. Refused once shutdown
    /// began. The count is raised before `closed` is read, and shutdown sets
    /// `closed` before it reads the count, so either shutdown waits for this
    /// dispatch or this dispatch sees the table closed.
    pub(super) fn enter_dispatch(&self) -> Result<DispatchInFlight<'_>, EngineError> {
        self.dispatches_in_flight.fetch_add(1, Ordering::SeqCst);
        let entered = DispatchInFlight(self);
        if self.closed.load(Ordering::SeqCst) {
            // Refused before any provider I/O: a queued message is released
            // for review, never marked as an unknown delivery.
            return Err(EngineError::DispatchPreflight(
                "This LUNA engine is shutting down and starts no new runs.".into(),
            ));
        }
        Ok(entered)
    }

    /// Refuse every later dispatch. Called before shutdown's first sweep.
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub(super) fn dispatches_in_flight(&self) -> usize {
        self.dispatches_in_flight.load(Ordering::SeqCst)
    }

    pub(super) fn transition_gate(&self, chat_id: &str) -> Arc<AsyncMutex<()>> {
        lock(&self.transition_gates)
            .entry(chat_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub(super) fn forget_transition_gate(&self, chat_id: &str) {
        lock(&self.transition_gates).remove(chat_id);
    }
}

/// One admitted dispatch, counted until it returns or is dropped.
pub(super) struct DispatchInFlight<'a>(&'a RunTable);

impl Drop for DispatchInFlight<'_> {
    fn drop(&mut self) {
        self.0.dispatches_in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

// ── AW-10 host turn capacity ────────────────────────────────────────────────

/// How often a waiting start's owner re-checks for a free slot.
pub(crate) const TURN_CAPACITY_RECHECK: Duration = Duration::from_secs(1);
/// A waiter not re-checked for this long has gone (its command was cancelled
/// or expired, its row removed, its document closed) and leaves the line, so
/// it can never hold back the starts behind it.
const TURN_WAITER_TTL_MS: i64 = 5_000;
/// How long an admitted start still counts as "just admitted" for readers
/// that give a start a grace before calling it unconfirmed (Automations and
/// Board use 5 s). After it, their own grace rule applies unchanged.
const TURN_START_GRACE_MS: i64 = 5_000;

/// The host's bound on concurrently active turns and the line of new turns
/// waiting for a slot. Admission, the starting count and the line change
/// under one lock, so two starts can never both take the last slot.
#[derive(Default)]
pub(crate) struct TurnCapacity {
    /// `None`: no limit (the default).
    limit: Mutex<Option<u32>>,
    /// Where the limit persists. Also serializes saves.
    path: Mutex<Option<PathBuf>>,
    line: Mutex<TurnLine>,
}

#[derive(Default)]
struct TurnLine {
    /// task → starts admitted whose turn has not registered yet.
    starting: HashMap<String, Starting>,
    /// Waiting starts, oldest issue first.
    waiting: Vec<TurnWaiter>,
}

struct Starting {
    count: usize,
    admitted_ms: i64,
}

struct TurnWaiter {
    task_id: String,
    kind: HostCapacityWaitKind,
    source_id: String,
    since_ms: i64,
    seen_ms: i64,
}

impl TurnWaiter {
    fn is(&self, task_id: &str, kind: HostCapacityWaitKind, source_id: &str) -> bool {
        self.task_id == task_id && self.kind == kind && self.source_id == source_id
    }

    fn fresh(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.seen_ms) <= TURN_WAITER_TTL_MS
    }
}

/// Admission's answer for one start.
pub(crate) enum TurnAdmission {
    /// Start now. The slot counts until the value drops, which its owner does
    /// once the turn has registered (or failed before registering).
    Admitted(TurnStartSlot),
    /// The host is full. Nothing may be written, claimed or sent; the start
    /// keeps its 1-based `position` while its owner re-checks.
    Waiting {
        position: u32,
        occupied: u32,
        limit: u32,
    },
}

/// One held start slot.
pub(crate) struct TurnStartSlot {
    capacity: Option<Arc<TurnCapacity>>,
    task_id: String,
}

impl TurnStartSlot {
    /// A start that never waits (a task whose turn is already active, a
    /// delegated child chat): it is counted through its active status alone.
    pub(crate) fn unbounded() -> Self {
        Self {
            capacity: None,
            task_id: String::new(),
        }
    }
}

impl Drop for TurnStartSlot {
    fn drop(&mut self) {
        let Some(capacity) = self.capacity.take() else {
            return;
        };
        let mut line = lock(&capacity.line);
        if let Some(start) = line.starting.get_mut(&self.task_id) {
            start.count = start.count.saturating_sub(1);
            if start.count == 0 {
                line.starting.remove(&self.task_id);
            }
        }
    }
}

/// `host-capacity.json` in the profile's store.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapacityFile {
    #[serde(default)]
    turn_limit: Option<u32>,
}

fn valid_limit(limit: u32) -> bool {
    (1..=HOST_TURN_LIMIT_MAX).contains(&limit)
}

/// Slots in use: every active task once, plus each starting task that is not
/// active yet.
fn occupied(active: &HashSet<String>, starting: &HashMap<String, Starting>) -> usize {
    active.len()
        + starting
            .keys()
            .filter(|task| !active.contains(*task))
            .count()
}

/// "1st", "2nd", "3rd", "4th", … for a 1-based place in line.
pub(crate) fn place_in_line(position: u32) -> String {
    let suffix = match (position % 10, position % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{position}{suffix}")
}

impl TurnCapacity {
    /// Read the persisted limit. A missing, unreadable or out-of-range file
    /// means no limit, which is the behaviour before AW-10.
    pub(crate) fn open(&self, path: PathBuf) {
        let limit = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<CapacityFile>(&bytes) {
                Ok(file) => file.turn_limit.filter(|limit| valid_limit(*limit)),
                Err(error) => {
                    tracing::warn!(%error, "host capacity file unreadable; no turn limit");
                    None
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(%error, "host capacity file unreadable; no turn limit");
                None
            }
        };
        *lock(&self.limit) = limit;
        *lock(&self.path) = Some(path);
    }

    pub(crate) fn limit(&self) -> Option<u32> {
        *lock(&self.limit)
    }

    /// Persist, then apply, a new limit. Lowering it never stops a running
    /// turn; raising it lets waiting starts in on their next re-check.
    pub(crate) fn set_limit(&self, limit: Option<u32>) -> Result<(), EngineError> {
        if limit.is_some_and(|limit| !valid_limit(limit)) {
            return Err(EngineError::Other(format!(
                "Choose a turn limit from 1 to {HOST_TURN_LIMIT_MAX}, or no limit"
            )));
        }
        let path = lock(&self.path);
        if let Some(path) = path.as_ref() {
            let parent = path.parent().ok_or_else(|| {
                EngineError::Other("host capacity path has no parent directory".into())
            })?;
            std::fs::create_dir_all(parent)?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer_pretty(
                temporary.as_file_mut(),
                &CapacityFile { turn_limit: limit },
            )?;
            temporary.as_file_mut().sync_all()?;
            temporary.persist(path).map_err(|error| error.error)?;
        }
        *lock(&self.limit) = limit;
        Ok(())
    }

    /// Admit one start of `task_id`, or give it (or keep) its place in line.
    /// `active` holds the tasks whose turn is running or parked on a
    /// question. A task already starting is admitted without another slot:
    /// at most one turn runs per task. Waiters are ordered by `since_ms`, the
    /// time their work was issued.
    pub(crate) fn admit(
        self: &Arc<Self>,
        task_id: &str,
        kind: HostCapacityWaitKind,
        source_id: &str,
        since_ms: i64,
        active: &HashSet<String>,
        now_ms: i64,
    ) -> TurnAdmission {
        let limit = self.limit();
        let mut line = lock(&self.line);
        line.waiting.retain(|waiter| waiter.fresh(now_ms));
        let index = line
            .waiting
            .iter()
            .position(|waiter| waiter.is(task_id, kind, source_id));
        let running = active.contains(task_id) || line.starting.contains_key(task_id);
        let occupied = occupied(active, &line.starting);
        let position = index.unwrap_or_else(|| {
            line.waiting
                .iter()
                .take_while(|waiter| waiter.since_ms <= since_ms)
                .count()
        });
        if running || limit.is_none_or(|limit| occupied + position < limit as usize) {
            if let Some(index) = index {
                line.waiting.remove(index);
            }
            let start = line.starting.entry(task_id.to_owned()).or_insert(Starting {
                count: 0,
                admitted_ms: now_ms,
            });
            start.count += 1;
            start.admitted_ms = now_ms;
            return TurnAdmission::Admitted(TurnStartSlot {
                capacity: Some(Arc::clone(self)),
                task_id: task_id.to_owned(),
            });
        }
        match index {
            Some(index) => line.waiting[index].seen_ms = now_ms,
            None => line.waiting.insert(
                position,
                TurnWaiter {
                    task_id: task_id.to_owned(),
                    kind,
                    source_id: source_id.to_owned(),
                    since_ms,
                    seen_ms: now_ms,
                },
            ),
        }
        TurnAdmission::Waiting {
            position: position as u32 + 1,
            occupied: occupied as u32,
            limit: limit.unwrap_or_default(),
        }
    }

    /// Whether this exact start still has a place in line.
    pub(crate) fn is_waiting(
        &self,
        task_id: &str,
        kind: HostCapacityWaitKind,
        source_id: &str,
        now_ms: i64,
    ) -> bool {
        lock(&self.line)
            .waiting
            .iter()
            .any(|waiter| waiter.is(task_id, kind, source_id) && waiter.fresh(now_ms))
    }

    /// Whether any start of this kind for `task_id` waits.
    pub(crate) fn task_waiting(
        &self,
        task_id: &str,
        kind: HostCapacityWaitKind,
        now_ms: i64,
    ) -> bool {
        lock(&self.line)
            .waiting
            .iter()
            .any(|waiter| waiter.task_id == task_id && waiter.kind == kind && waiter.fresh(now_ms))
    }

    /// Leave the line: the waiting work was withdrawn, or started another way.
    /// `None` removes every start of this kind for the task.
    pub(crate) fn forget(
        &self,
        task_id: &str,
        kind: HostCapacityWaitKind,
        source_id: Option<&str>,
    ) {
        lock(&self.line).waiting.retain(|waiter| {
            !(waiter.task_id == task_id
                && waiter.kind == kind
                && source_id.is_none_or(|id| waiter.source_id == id))
        });
    }

    /// A send of `task_id` waits in, or was just admitted from, the line.
    pub(crate) fn send_pending(&self, task_id: &str, now_ms: i64) -> bool {
        let line = lock(&self.line);
        line.starting
            .get(task_id)
            .is_some_and(|start| now_ms.saturating_sub(start.admitted_ms) <= TURN_START_GRACE_MS)
            || line.waiting.iter().any(|waiter| {
                waiter.task_id == task_id
                    && waiter.kind == HostCapacityWaitKind::Send
                    && waiter.fresh(now_ms)
            })
    }

    /// Readback: the limit, the starting tasks not yet active, and the line.
    pub(crate) fn snapshot(
        &self,
        active: &HashSet<String>,
        now_ms: i64,
    ) -> (Option<u32>, u32, Vec<HostCapacityWait>) {
        let limit = self.limit();
        let line = lock(&self.line);
        let starting = line
            .starting
            .keys()
            .filter(|task| !active.contains(*task))
            .count() as u32;
        let waiting = line
            .waiting
            .iter()
            .filter(|waiter| waiter.fresh(now_ms))
            .enumerate()
            .map(|(index, waiter)| HostCapacityWait {
                task_id: waiter.task_id.clone(),
                kind: waiter.kind,
                source_id: waiter.source_id.clone(),
                position: index as u32 + 1,
                since_ms: waiter.since_ms,
            })
            .collect();
        (limit, starting, waiting)
    }
}
