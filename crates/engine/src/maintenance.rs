//! One supervisor for the engine's background work (CORE-03).
//!
//! The engine's local RPC service starts its maintenance once: cleanup,
//! child-thread, automation and Board review/lifecycle sweeps, the
//! session-history indexer and, on a copy-isolated root, the backup schedule.
//! Every task is spawned into one tracker under one cancellation token, so
//! shutdown cancels all of them and waits for each to finish.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// What a maintenance task is spawned into.
#[derive(Clone)]
pub(crate) struct WorkerScope {
    pub(crate) cancel: CancellationToken,
    tracker: TaskTracker,
}

impl WorkerScope {
    /// Spawns onto the current runtime; callers check for one first.
    pub(crate) fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tracker.spawn(task);
    }
}

pub(crate) struct Maintenance {
    scope: WorkerScope,
    started: AtomicBool,
}

impl Maintenance {
    pub(crate) fn new() -> Self {
        Self {
            scope: WorkerScope {
                cancel: CancellationToken::new(),
                tracker: TaskTracker::new(),
            },
            started: AtomicBool::new(false),
        }
    }

    /// The one start: `Some` exactly once, and never after cancellation.
    pub(crate) fn claim_start(&self) -> Option<&WorkerScope> {
        if self.scope.cancel.is_cancelled() || self.started.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(&self.scope)
    }

    pub(crate) fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Cancels every task, then waits up to `timeout` for them to finish.
    /// Returns how many were still running; each stops at its next
    /// cancellation point.
    pub(crate) async fn stop(&self, timeout: Duration) -> usize {
        self.cancel();
        if tokio::time::timeout(timeout, self.scope.tracker.wait())
            .await
            .is_err()
        {
            return self.scope.tracker.len();
        }
        0
    }

    /// The drop path: cancel without waiting.
    pub(crate) fn cancel(&self) {
        self.scope.cancel.cancel();
        self.scope.tracker.close();
    }
}
