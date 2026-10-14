//! Native GPUI platforms for unit tests. Construct every one through here,
//! never through `gpui_platform` directly.
//!
//! libtest runs tests on worker threads, and on macOS a platform's
//! construction makes HIToolbox's first keyboard-layout lookup
//! (`TISCopyCurrentKeyboardLayoutInputSource`). That first lookup builds a
//! process-wide input-source cache that is not thread-safe: two tests that
//! construct a platform at the same moment abort the whole test process with
//! SIGABRT and no Rust panic, taking every unfinished test down with it.
//!
//! Failure cases this must avoid (written before the implementation):
//! 1. Two platforms constructed concurrently on test threads abort the run.
//! 2. Holding the lock while a platform runs serializes whole tests, or
//!    deadlocks a test that builds a second platform inside the first's run.
//! 3. One constructor panicking poisons the lock and fails every later
//!    native test.
use std::rc::Rc;
use std::sync::Mutex;

/// Held only while a platform is constructed, never while it runs.
static CONSTRUCTION: Mutex<()> = Mutex::new(());

fn serialized<T>(construct: impl FnOnce() -> T) -> T {
    let _guard = CONSTRUCTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    construct()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn headless() -> gpui::Application {
    serialized(gpui_platform::headless)
}

pub(crate) fn background_executor() -> gpui::BackgroundExecutor {
    serialized(gpui_platform::background_executor)
}

pub(crate) fn current_platform(headless: bool) -> Rc<dyn gpui::Platform> {
    serialized(|| gpui_platform::current_platform(headless))
}
