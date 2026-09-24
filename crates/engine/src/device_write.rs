//! Engine-owned lifetime for the device-root write coordinator.
//!
//! A ticket owns the InstanceLock
//! lease until its epoch count is released, even if EngineCore is dropped.

#![allow(dead_code)] // Internal lifecycle prerequisite; product writers are unwired.

use std::io;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use luna_sync::write_epoch::{EpochError, WriteEpoch, WritePermit};

use crate::InstanceLock;

struct Inner {
    epoch: WriteEpoch,
    _instance_lock: InstanceLock,
}

/// The only strong lifetime owner outside admitted tickets.
pub(crate) struct DeviceWriteOwner {
    inner: Arc<Inner>,
}

impl DeviceWriteOwner {
    pub(crate) fn new(root: &Path, lock: InstanceLock) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                epoch: WriteEpoch::for_device_root(root)?,
                _instance_lock: lock,
            }),
        })
    }

    pub(crate) fn handle(&self) -> DeviceWriteHandle {
        DeviceWriteHandle {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.inner.epoch.root()
    }

    pub(crate) fn retire(&self) {
        self.inner.epoch.retire();
    }

    #[cfg(test)]
    pub(crate) fn epoch_for_tests(&self) -> WriteEpoch {
        self.inner.epoch.clone()
    }
}

impl Drop for DeviceWriteOwner {
    fn drop(&mut self) {
        self.retire();
    }
}

/// A stale component clone cannot keep the InstanceLock alive or admit work.
#[derive(Clone)]
pub(crate) struct DeviceWriteHandle {
    inner: Weak<Inner>,
}

impl DeviceWriteHandle {
    pub(crate) fn owns(&self, permit: &DeviceWritePermit) -> bool {
        self.inner
            .upgrade()
            .is_some_and(|lease| Arc::ptr_eq(&lease, &permit._lease))
    }

    pub(crate) fn try_admit(&self) -> Result<DeviceWritePermit, EpochError> {
        let lease = self.inner.upgrade().ok_or(EpochError::Retired)?;
        let ticket = lease.epoch.try_admit()?;
        Ok(DeviceWritePermit {
            ticket,
            _lease: lease,
        })
    }

    pub(crate) async fn admit_async(
        &self,
        timeout: Duration,
    ) -> Result<DeviceWritePermit, EpochError> {
        let lease = self.inner.upgrade().ok_or(EpochError::Retired)?;
        let ticket = lease.epoch.admit_async(timeout).await?;
        Ok(DeviceWritePermit {
            ticket,
            _lease: lease,
        })
    }
}

/// Field order drops the epoch ticket before the OS lock's last strong lease.
pub(crate) struct DeviceWritePermit {
    ticket: WritePermit,
    _lease: Arc<Inner>,
}

impl DeviceWritePermit {
    pub(crate) fn ticket(&self) -> &WritePermit {
        &self.ticket
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_sync::write_epoch::Phase;

    #[test]
    fn old_ticket_holds_os_lock_until_released_then_fresh_epoch_starts() {
        let root = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(root.path()).unwrap();
        let owner = DeviceWriteOwner::new(root.path(), lock).unwrap();
        let stale = owner.handle();
        let ticket = stale.try_admit().unwrap();
        assert_eq!(ticket.ticket().epoch(), 1);
        owner.retire();
        drop(owner);

        assert!(matches!(stale.try_admit(), Err(EpochError::Retired)));
        // InstanceLock's bounded contention probe may take up to one second.
        assert!(InstanceLock::acquire(root.path()).is_err());
        drop(ticket);

        let replacement_lock = InstanceLock::acquire(root.path()).unwrap();
        let replacement = DeviceWriteOwner::new(root.path(), replacement_lock).unwrap();
        assert_eq!(
            replacement.handle().try_admit().unwrap().ticket().epoch(),
            1
        );
        replacement.retire();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retirement_wakes_waiting_handle_and_freeze_cannot_reopen() {
        let root = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(root.path()).unwrap();
        let owner = DeviceWriteOwner::new(root.path(), lock).unwrap();
        let handle = owner.handle();
        let first = handle.try_admit().unwrap();

        let mut freezing = Box::pin(owner.inner.epoch.freeze(Duration::from_secs(2)));
        assert!(matches!(
            futures::poll!(freezing.as_mut()),
            std::task::Poll::Pending
        ));
        assert_eq!(owner.inner.epoch.phase(), Phase::Quiescing);
        let mut waiting = Box::pin(handle.admit_async(Duration::from_secs(2)));
        assert!(matches!(
            futures::poll!(waiting.as_mut()),
            std::task::Poll::Pending
        ));

        owner.retire();
        assert!(matches!(waiting.await, Err(EpochError::Retired)));
        drop(freezing);
        drop(first);
        assert_eq!(owner.inner.epoch.phase(), Phase::Retired);
        assert!(matches!(handle.try_admit(), Err(EpochError::Retired)));
    }

    #[test]
    fn owner_drop_retires_admission_without_an_explicit_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let owner = DeviceWriteOwner::new(root.path(), InstanceLock::acquire(root.path()).unwrap())
            .unwrap();
        let stale = owner.handle();
        drop(owner);
        assert!(matches!(stale.try_admit(), Err(EpochError::Retired)));
        InstanceLock::acquire(root.path()).unwrap();
    }
}
