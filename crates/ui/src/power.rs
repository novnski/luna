//! Opt-in idle-sleep assertion for one selected, actively working local task.
//! Closing a lid, quitting the engine, or losing the network is never treated
//! as continued execution. The assertion is process-local and RAII released.

use luna_proto::{Session, view::Indicator};

pub(crate) trait PowerAdapter {
    type Handle;
    fn acquire(&mut self) -> Result<Self::Handle, String>;
    fn release(&mut self, handle: Self::Handle);
}

pub(crate) struct KeepAwake<A: PowerAdapter> {
    adapter: A,
    held: Option<(String, A::Handle)>,
}

impl<A: PowerAdapter> KeepAwake<A> {
    pub(crate) fn new(adapter: A) -> Self {
        Self {
            adapter,
            held: None,
        }
    }

    pub(crate) fn reconcile(&mut self, task_id: Option<&str>) -> Result<(), String> {
        if self.held.as_ref().map(|(id, _)| id.as_str()) == task_id {
            return Ok(());
        }
        if let Some((_, handle)) = self.held.take() {
            self.adapter.release(handle);
        }
        if let Some(task_id) = task_id {
            let handle = self.adapter.acquire()?;
            self.held = Some((task_id.into(), handle));
        }
        Ok(())
    }

    #[cfg(test)]
    fn held_task(&self) -> Option<&str> {
        self.held.as_ref().map(|(id, _)| id.as_str())
    }
}

impl<A: PowerAdapter> Drop for KeepAwake<A> {
    fn drop(&mut self) {
        if let Some((_, handle)) = self.held.take() {
            self.adapter.release(handle);
        }
    }
}

pub(crate) fn selected_active_task<'a>(
    selected: Option<&'a str>,
    enabled: &std::collections::HashSet<String>,
    local_device_id: Option<&str>,
    sessions: &[Session],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'a str> {
    let id = selected.filter(|id| enabled.contains(*id))?;
    let session = sessions.iter().find(|session| session.chat_id == id)?;
    (Some(session.device_id.as_str()) == local_device_id
        && luna_proto::view::effective_indicator(Some(session), now) == Indicator::Working)
        .then_some(id)
}

#[derive(Default)]
pub(crate) struct SystemPower;

impl PowerAdapter for SystemPower {
    type Handle = u32;

    fn acquire(&mut self) -> Result<Self::Handle, String> {
        if std::env::var_os("LUNA_FIXTURE_FAKE_POWER").is_some() {
            return Ok(0);
        }
        #[cfg(target_os = "macos")]
        {
            macos::acquire()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("Keep awake is unavailable on this platform.".into())
        }
    }

    fn release(&mut self, handle: Self::Handle) {
        if handle == 0 {
            return;
        }
        #[cfg(target_os = "macos")]
        macos::release(handle);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_char, c_void};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            name: *const c_void,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }

    const UTF8: u32 = 0x0800_0100;

    pub(super) fn acquire() -> Result<u32, String> {
        unsafe {
            let assertion_type =
                CFStringCreateWithCString(std::ptr::null(), c"NoIdleSleepAssertion".as_ptr(), UTF8);
            let name = CFStringCreateWithCString(
                std::ptr::null(),
                c"LUNA selected active task".as_ptr(),
                UTF8,
            );
            if assertion_type.is_null() || name.is_null() {
                if !assertion_type.is_null() {
                    CFRelease(assertion_type);
                }
                if !name.is_null() {
                    CFRelease(name);
                }
                return Err("Could not create keep-awake assertion labels.".into());
            }
            let mut assertion_id = 0;
            let status = IOPMAssertionCreateWithName(assertion_type, 255, name, &mut assertion_id);
            CFRelease(assertion_type);
            CFRelease(name);
            if status != 0 || assertion_id == 0 {
                return Err(format!(
                    "macOS rejected the idle-sleep assertion ({status})."
                ));
            }
            Ok(assertion_id)
        }
    }

    pub(super) fn release(assertion_id: u32) {
        let status = unsafe { IOPMAssertionRelease(assertion_id) };
        if status != 0 {
            tracing::warn!(status, "could not release LUNA idle-sleep assertion");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct FakePower(Arc<Mutex<Vec<&'static str>>>, bool);

    impl PowerAdapter for FakePower {
        type Handle = u32;
        fn acquire(&mut self) -> Result<Self::Handle, String> {
            self.0.lock().unwrap().push("acquire");
            if self.1 {
                Err("injected".into())
            } else {
                Ok(1)
            }
        }
        fn release(&mut self, _: Self::Handle) {
            self.0.lock().unwrap().push("release");
        }
    }

    #[test]
    fn acquire_release_switch_error_and_drop() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut guard = KeepAwake::new(FakePower(events.clone(), false));
        guard.reconcile(Some("a")).unwrap();
        guard.reconcile(Some("a")).unwrap();
        guard.reconcile(Some("b")).unwrap();
        guard.adapter.1 = true;
        assert!(guard.reconcile(Some("c")).is_err());
        assert_eq!(guard.held_task(), None);
        guard.adapter.1 = false;
        guard.reconcile(Some("d")).unwrap();
        drop(guard);
        assert_eq!(
            *events.lock().unwrap(),
            [
                "acquire", "release", "acquire", "release", "acquire", "acquire", "release"
            ]
        );
    }

    #[test]
    fn selection_requires_opt_in_fresh_local_working_status() {
        let now = chrono::Utc::now();
        let mut session = Session {
            chat_id: "task".into(),
            device_id: "local".into(),
            status: luna_proto::SessionStatus::Working,
            started_at: Some(now),
            updated_at: now,
            last_completed_turn: None,
            recovery: None,
        };
        let enabled = std::collections::HashSet::from(["task".to_string()]);
        assert_eq!(
            selected_active_task(
                Some("task"),
                &enabled,
                Some("local"),
                &[session.clone()],
                now
            ),
            Some("task")
        );
        assert_eq!(
            selected_active_task(
                Some("other"),
                &enabled,
                Some("local"),
                &[session.clone()],
                now
            ),
            None
        );
        assert_eq!(
            selected_active_task(
                Some("task"),
                &enabled,
                Some("remote"),
                &[session.clone()],
                now
            ),
            None
        );
        session.status = luna_proto::SessionStatus::Idle;
        assert_eq!(
            selected_active_task(
                Some("task"),
                &enabled,
                Some("local"),
                &[session.clone()],
                now
            ),
            None
        );
        session.status = luna_proto::SessionStatus::Working;
        session.updated_at = now - chrono::Duration::seconds(60);
        assert_eq!(
            selected_active_task(Some("task"), &enabled, Some("local"), &[session], now),
            None
        );
    }
}
