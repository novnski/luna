//! CORE-11: the worker of LUNA's first-party OpenCode provider package
//! (`luna.provider/opencode`). It hosts LUNA's own OpenCode 2 adapter
//! (`luna_harness::OpencodeHarness`, HTTP + SSE to `opencode serve
//! --stdio`) and serves it over provider profile 2.0 on its stdio. The
//! engine installs it as an ordinary package from next to LUNA's own
//! executable and runs it as a supervised CORE-07 native worker: one
//! instance per turn (which owns that turn's `opencode serve` process and
//! port) and one for model discovery and the other out-of-turn operations.
//!
//! It is user-level native code with the user's account access; LUNA does
//! not sandbox it.

use std::process::ExitCode;
use std::sync::Arc;

use luna_harness::OpencodeHarness;
use luna_harness::hosted::worker::{self, StateHooks, WorkerSpec};
use serde_json::Value;

type ExecutableProbe = Arc<dyn Fn() -> Option<(String, Option<String>)> + Send + Sync>;

const UNAVAILABLE: &str = "OpenCode is not installed on this device. Install OpenCode 2 (`curl -fsSL https://opencode.ai/install | bash`), then sign in with `opencode auth login`.";

fn main() -> ExitCode {
    // The adapter adds the login shell's PATH to every `opencode serve`.
    luna_harness::shell_env::prewarm();
    let harness = Arc::new(OpencodeHarness::new());
    let exports = harness.clone();
    let imports = harness.clone();
    let executable: ExecutableProbe = Arc::new(|| {
        luna_harness::opencode::resolve_opencode_executable()?;
        let resolution = luna_harness::opencode::last_opencode_resolution()?;
        Some((resolution.path.display().to_string(), resolution.version))
    });
    worker::run(WorkerSpec {
        provider_id: "luna.provider/opencode".into(),
        harness,
        executable: Some(executable),
        unavailable_reason: UNAVAILABLE.into(),
        // LUNA's "Allow for session" grants outlive one turn's worker.
        state: Some(StateHooks {
            export: Arc::new(move || exports.export_session_grants()),
            import: Arc::new(move |state: &Value| imports.import_session_grants(state)),
        }),
    })
}
