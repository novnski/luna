//! The live-run table: the one owner of "may a run start" (CORE-03).
//!
//! It holds every chat's live run, counts dispatches between admission and
//! run registration, keeps each chat's transition gate, and closes when
//! shutdown begins. A dispatch enters through [`RunTable::enter_dispatch`]:
//! once the table is closed it is refused before it reaches a provider, so
//! no run can register after shutdown's final sweep and outlive the engine.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

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
}

impl RunTable {
    pub(super) fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            dispatches_in_flight: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            transition_gates: Mutex::new(HashMap::new()),
        }
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
