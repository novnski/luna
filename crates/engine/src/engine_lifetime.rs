//! The engine's command admission and lifetime owner (CORE-03).
//!
//! Every `EngineRpc` an engine hands out shares this one gate. A catalogued
//! `Command` is admitted before its handler runs. Admission closes when
//! shutdown begins, or when the engine drops, and never reopens: a client kept
//! from a stopped engine can't write behind the engine that replaced it. An
//! admitted command holds the device lease (the InstanceLock) until it
//! returns, so a replacement engine cannot open the same root alongside it.
//! Queries and watches are not gated here.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use luna_rpc::RpcError;
use tokio::sync::watch;

use crate::device_write::{DeviceLease, DeviceWriteHandle};

struct State {
    open: bool,
    in_flight: usize,
}

struct Inner {
    state: Mutex<State>,
    /// The in-flight count, for shutdown's drain.
    drained: watch::Sender<usize>,
    writes: DeviceWriteHandle,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn stopped() -> RpcError {
    RpcError::Failed(
        "This LUNA engine has stopped and accepts no new commands. Reconnect to the running engine."
            .into(),
    )
}

/// Owned by `EngineCore`; dropping it closes admission.
pub(crate) struct EngineLifetime {
    inner: Arc<Inner>,
}

impl EngineLifetime {
    pub(crate) fn new(writes: DeviceWriteHandle) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    open: true,
                    in_flight: 0,
                }),
                drained: watch::channel(0).0,
                writes,
            }),
        }
    }

    /// The gate every `EngineRpc` of this engine checks.
    pub(crate) fn admission(&self) -> CommandAdmission {
        CommandAdmission {
            inner: self.inner.clone(),
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.inner.lock().open
    }

    pub(crate) fn close(&self) {
        self.inner.lock().open = false;
    }

    /// Closes admission, then waits up to `timeout` for admitted commands to
    /// return. Returns how many were still running.
    pub(crate) async fn close_and_drain(&self, timeout: Duration) -> usize {
        self.close();
        let mut drained = self.inner.drained.subscribe();
        let wait = drained.wait_for(|in_flight| *in_flight == 0);
        if tokio::time::timeout(timeout, wait).await.is_err() {
            return self.inner.lock().in_flight;
        }
        0
    }
}

impl Drop for EngineLifetime {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone)]
pub(crate) struct CommandAdmission {
    inner: Arc<Inner>,
}

impl CommandAdmission {
    pub(crate) fn admit(&self) -> Result<AdmittedCommand, RpcError> {
        let mut state = self.inner.lock();
        if !state.open {
            return Err(stopped());
        }
        let lease = self.inner.writes.lease().ok_or_else(stopped)?;
        state.in_flight += 1;
        self.inner.drained.send_replace(state.in_flight);
        Ok(AdmittedCommand {
            inner: self.inner.clone(),
            _lease: lease,
        })
    }
}

/// Held for the whole handler, and for the life of a streamed reply.
pub(crate) struct AdmittedCommand {
    inner: Arc<Inner>,
    _lease: DeviceLease,
}

impl Drop for AdmittedCommand {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        state.in_flight -= 1;
        self.inner.drained.send_replace(state.in_flight);
    }
}
