//! Device-root write admission for a future coordinated backup snapshot.
//!
//! This module and the guarded snapshot-store seam cannot make a live backup
//! consistent until every authoritative writer takes its permit. The lock
//! order is: engine InstanceLock (lifetime) -> WritePermit ->
//! component mutex/file lock/SQLite transaction. Callers must acquire a
//! permit before component locks and pass it to nested writes; never wait for
//! a permit while holding one of those locks. Synchronous callers use the
//! nonblocking `try_admit`; async callers may wait without blocking a runtime
//! thread. A freeze owns an exclusive flush phase before snapshot capture.

#![allow(dead_code)] // Foundation only; no product writer is connected yet.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Open,
    Quiescing,
    Flushing,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochError {
    Busy,
    TimedOut,
    Exhausted,
    Stale,
}

struct State {
    phase: Phase,
    active_writers: usize,
    epoch: u64,
    revision: u64,
}

struct Inner {
    root: PathBuf,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self, state: &mut State) {
        state.revision = state.revision.wrapping_add(1);
        self.changed.send_replace(state.revision);
    }

    fn release_freeze(&self, epoch: u64, phase: Phase) {
        let mut state = self.lock();
        if state.epoch != epoch || state.phase != phase {
            return;
        }
        state.phase = Phase::Open;
        // A failed drain may still have writers from this epoch in flight.
        // Advance only after a completed drain has entered exclusive flush.
        if phase != Phase::Quiescing {
            // Freeze refuses to start at MAX.
            state.epoch += 1;
        }
        self.notify(&mut state);
    }
}

/// One coordinator must be owned by the EngineCore for this device root.
/// Construction performs no write and does not acquire InstanceLock itself.
#[derive(Clone)]
pub struct WriteEpoch {
    inner: Arc<Inner>,
}

impl WriteEpoch {
    pub fn for_device_root(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write epoch root must be a directory",
            ));
        }
        let (changed, _) = watch::channel(0);
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                state: Mutex::new(State {
                    phase: Phase::Open,
                    active_writers: 0,
                    epoch: 1,
                    revision: 0,
                }),
                changed,
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn phase(&self) -> Phase {
        self.inner.lock().phase
    }

    /// Never waits: safe at a synchronous outer write boundary, including on
    /// a single-thread async runtime. Busy must be retried from outside locks.
    pub fn try_admit(&self) -> Result<WritePermit, EpochError> {
        let mut state = self.inner.lock();
        if state.phase != Phase::Open {
            return Err(EpochError::Busy);
        }
        state.active_writers = state
            .active_writers
            .checked_add(1)
            .ok_or(EpochError::Exhausted)?;
        let epoch = state.epoch;
        self.inner.notify(&mut state);
        Ok(WritePermit {
            inner: self.inner.clone(),
            epoch,
        })
    }

    /// Waits without holding a mutex or blocking a runtime thread. Dropping
    /// the future cancels only this admission request, not an admitted write.
    pub async fn admit_async(&self, timeout: Duration) -> Result<WritePermit, EpochError> {
        let deadline = Instant::now() + timeout;
        let mut changed = self.inner.changed.subscribe();
        loop {
            match self.try_admit() {
                Ok(permit) => return Ok(permit),
                Err(EpochError::Busy) => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(EpochError::TimedOut);
            }
            tokio::select! {
                result = changed.changed() => {
                    if result.is_err() { return Err(EpochError::Stale); }
                }
                _ = tokio::time::sleep_until(deadline) => return Err(EpochError::TimedOut),
            }
        }
    }

    /// Close admission, drain already admitted writers, and enter an
    /// exclusive flush phase. A timeout or dropped future reopens admission.
    /// No snapshot epoch exists until `FlushGuard::into_snapshot` succeeds.
    pub async fn freeze(&self, timeout: Duration) -> Result<FlushGuard, EpochError> {
        let deadline = Instant::now() + timeout;
        let mut changed = self.inner.changed.subscribe();
        let mut attempt = {
            let mut state = self.inner.lock();
            if state.phase != Phase::Open {
                return Err(EpochError::Busy);
            }
            if state.epoch == u64::MAX {
                return Err(EpochError::Exhausted);
            }
            state.phase = Phase::Quiescing;
            self.inner.notify(&mut state);
            FreezeAttempt {
                inner: self.inner.clone(),
                epoch: state.epoch,
                armed: true,
            }
        };
        loop {
            if let Some(guard) = attempt.enter_flush()? {
                return Ok(guard);
            }
            if Instant::now() >= deadline {
                return Err(EpochError::TimedOut);
            }
            tokio::select! {
                result = changed.changed() => {
                    if result.is_err() { return Err(EpochError::Stale); }
                }
                _ = tokio::time::sleep_until(deadline) => return Err(EpochError::TimedOut),
            }
        }
    }

    pub(crate) fn permits_write(&self, permit: &WritePermit) -> bool {
        if !Arc::ptr_eq(&self.inner, &permit.inner) {
            return false;
        }
        let state = self.inner.lock();
        state.epoch == permit.epoch && matches!(state.phase, Phase::Open | Phase::Quiescing)
    }

    pub(crate) fn permits_flush(&self, permit: &FlushPermit<'_>) -> bool {
        if !Arc::ptr_eq(&self.inner, &permit.guard.inner) {
            return false;
        }
        let state = self.inner.lock();
        state.epoch == permit.guard.epoch && state.phase == Phase::Flushing
    }
}

/// One admitted semantic write. Nested file/DB writes use this same token.
pub struct WritePermit {
    inner: Arc<Inner>,
    epoch: u64,
}

impl WritePermit {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }
}

impl Drop for WritePermit {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        debug_assert!(state.active_writers > 0);
        state.active_writers -= 1;
        self.inner.notify(&mut state);
    }
}

struct FreezeAttempt {
    inner: Arc<Inner>,
    epoch: u64,
    armed: bool,
}

impl FreezeAttempt {
    fn enter_flush(&mut self) -> Result<Option<FlushGuard>, EpochError> {
        let mut state = self.inner.lock();
        if state.epoch != self.epoch || state.phase != Phase::Quiescing {
            return Err(EpochError::Stale);
        }
        if state.active_writers != 0 {
            return Ok(None);
        }
        state.phase = Phase::Flushing;
        self.inner.notify(&mut state);
        self.armed = false;
        Ok(Some(FlushGuard {
            inner: self.inner.clone(),
            epoch: self.epoch,
            armed: true,
        }))
    }
}

impl Drop for FreezeAttempt {
    fn drop(&mut self) {
        if self.armed {
            self.inner.release_freeze(self.epoch, Phase::Quiescing);
        }
    }
}

/// Exclusive phase for flushing debounced app state. Ordinary writers remain
/// blocked; future flush adapters receive only the borrowed `FlushPermit`.
pub struct FlushGuard {
    inner: Arc<Inner>,
    epoch: u64,
    armed: bool,
}

pub struct FlushPermit<'a> {
    guard: &'a FlushGuard,
}

impl FlushPermit<'_> {
    pub fn epoch(&self) -> u64 {
        self.guard.epoch
    }

    pub fn root(&self) -> &Path {
        &self.guard.inner.root
    }
}

impl FlushGuard {
    pub fn permit(&self) -> FlushPermit<'_> {
        FlushPermit { guard: self }
    }

    /// Called only after every registered writer has flushed. Consuming the
    /// flush guard makes privileged writes unavailable during snapshot copy.
    pub fn into_snapshot(mut self) -> Result<SnapshotGuard, EpochError> {
        {
            let mut state = self.inner.lock();
            if state.epoch != self.epoch || state.phase != Phase::Flushing {
                return Err(EpochError::Stale);
            }
            state.phase = Phase::Snapshot;
            self.inner.notify(&mut state);
        }
        self.armed = false;
        Ok(SnapshotGuard {
            inner: self.inner.clone(),
            epoch: self.epoch,
        })
    }
}

impl Drop for FlushGuard {
    fn drop(&mut self) {
        if self.armed {
            self.inner.release_freeze(self.epoch, Phase::Flushing);
        }
    }
}

/// Stable admission boundary, not a published backup receipt. Dropping this
/// guard releases waiting writers even if capture fails or is cancelled.
pub struct SnapshotGuard {
    inner: Arc<Inner>,
    epoch: u64,
}

impl SnapshotGuard {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }
}

impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        self.inner.release_freeze(self.epoch, Phase::Snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    async fn wait_for_phase(epoch: &WriteEpoch, expected: Phase) {
        let mut changed = epoch.inner.changed.subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            while epoch.phase() != expected {
                changed.changed().await.expect("coordinator still exists");
            }
        })
        .await
        .expect("phase transition timed out");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn freeze_drains_writers_and_separates_snapshot_from_next_epoch() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));

        let first = epoch.try_admit().unwrap();
        assert_eq!(first.epoch(), 1);
        assert_eq!(first.root(), epoch.root());

        let freezing = {
            let epoch = epoch.clone();
            tokio::spawn(async move { epoch.freeze(Duration::from_secs(2)).await })
        };
        wait_for_phase(&epoch, Phase::Quiescing).await;
        assert_eq!(epoch.try_admit().err(), Some(EpochError::Busy));

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let second = {
            let epoch = epoch.clone();
            let events = events.clone();
            tokio::spawn(async move {
                started_tx.send(()).unwrap();
                let permit = epoch.admit_async(Duration::from_secs(2)).await.unwrap();
                events.lock().unwrap().push("second");
                permit.epoch()
            })
        };
        started_rx.await.unwrap();
        events.lock().unwrap().push("first");
        drop(first);

        let flush = freezing.await.unwrap().unwrap();
        assert_eq!(epoch.phase(), Phase::Flushing);
        let privileged = flush.permit();
        assert_eq!(privileged.epoch(), 1);
        assert_eq!(privileged.root(), epoch.root());
        assert_eq!(epoch.try_admit().err(), Some(EpochError::Busy));

        let snapshot = flush.into_snapshot().unwrap();
        assert_eq!(epoch.phase(), Phase::Snapshot);
        assert_eq!(snapshot.epoch(), 1);
        assert_eq!(snapshot.root(), epoch.root());
        assert_eq!(*events.lock().unwrap(), ["first"]);
        assert!(!second.is_finished());
        drop(snapshot);

        assert_eq!(second.await.unwrap(), 2);
        assert_eq!(*events.lock().unwrap(), ["first", "second"]);
        assert_eq!(epoch.phase(), Phase::Open);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timed_out_or_cancelled_drain_reopens_without_advancing_epoch() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let first = epoch.try_admit().unwrap();

        assert!(matches!(
            epoch.freeze(Duration::ZERO).await,
            Err(EpochError::TimedOut)
        ));
        assert_eq!(epoch.phase(), Phase::Open);
        let after_timeout = epoch.try_admit().unwrap();
        assert_eq!(after_timeout.epoch(), first.epoch());
        drop(after_timeout);

        let freezing = {
            let epoch = epoch.clone();
            tokio::spawn(async move { epoch.freeze(Duration::from_secs(2)).await })
        };
        wait_for_phase(&epoch, Phase::Quiescing).await;
        freezing.abort();
        assert!(matches!(freezing.await, Err(error) if error.is_cancelled()));
        assert_eq!(epoch.phase(), Phase::Open);
        let after_cancel = epoch.try_admit().unwrap();
        assert_eq!(after_cancel.epoch(), first.epoch());
        drop(after_cancel);
        drop(first);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_flush_guard_reopens_admission() {
        let root = tempfile::tempdir().unwrap();
        let epoch = WriteEpoch::for_device_root(root.path()).unwrap();
        let flush = epoch.freeze(Duration::from_secs(1)).await.unwrap();
        assert_eq!(epoch.phase(), Phase::Flushing);
        assert_eq!(epoch.try_admit().err(), Some(EpochError::Busy));
        drop(flush);
        assert_eq!(epoch.phase(), Phase::Open);
        assert_eq!(epoch.try_admit().unwrap().epoch(), 2);
    }
}
