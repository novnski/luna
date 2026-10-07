//! Live probe: one full turn through the native driver against a chosen
//! opencode binary — prints the event stream and exits 0 only on a
//! Completed turn with text. Arg 1 = executable (default: PATH resolution),
//! arg 2 = model (`provider/model`), arg 3 = prompt.
//!
//!     cargo run -p luna-harness --example opencode_turn_probe -- \
//!         ~/.opencode/bin/opencode opencode/muse-spark-1.3-contributor-free \
//!         "Read /tmp/note.txt and reply with its secret word."
//!
//! After Done the probe keeps draining until the adapter closes the stream,
//! so the server teardown (interrupt of active sessions, lease release,
//! SIGTERM) completes inside the probe. Teardown probes:
//! - `PROBE_INTERRUPT_AFTER_MS=<ms>`: press Stop after the delay (expects
//!   an Interrupted turn; exit 0 on Interrupted).
//! - `PROBE_DROP_AFTER_MS=<ms>`: drop the event stream mid-turn, as a
//!   vanished consumer would, then wait for the adapter's teardown (exit 0).

use futures::StreamExt;
use luna_harness::{CancellationToken, Harness, OpencodeHarness, RunControls};
use luna_proto::{AgentEvent, RunRequest, SandboxLevel};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    let exe = std::env::args().nth(1);
    let model = std::env::args().nth(2).filter(|m| m.contains('/'));
    let prompt = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "Reply with exactly: PONG".into());
    let workspace = tempfile::tempdir().expect("create isolated probe workspace");
    let cwd = workspace.path().to_str().expect("UTF-8 workspace path");
    let (_steer_tx, steering) = mpsc::channel(8);
    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    let delay = |key: &str| {
        std::env::var(key)
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
    };
    let interrupt_after = delay("PROBE_INTERRUPT_AFTER_MS");
    let drop_after = delay("PROBE_DROP_AFTER_MS");
    let interrupt = CancellationToken::new();
    if let Some(after) = interrupt_after {
        let interrupt = interrupt.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            eprintln!("--- pressing Stop");
            interrupt.cancel();
        });
    }
    let started = std::time::Instant::now();
    let mut harness = OpencodeHarness::new();
    if let Some(exe) = exe {
        harness = harness.with_executable(exe);
    }
    let mut stream = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| panic!("probe must not ask for input")),
                steering,
                interrupt,
                invoke_luna_tool: None,
            },
        )
        .await
        .expect("run starts");
    let mut text = String::new();
    let mut tools = 0u32;
    let status = loop {
        if let Some(after) = drop_after
            && started.elapsed() >= after
        {
            eprintln!("--- dropping the event stream mid-turn");
            drop(stream);
            // The adapter notices on its next send and tears the server down.
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            print_resolution();
            return;
        }
        let ev =
            match tokio::time::timeout(std::time::Duration::from_secs(120), stream.next()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => break None,
                Err(_) => {
                    eprintln!("--- timed out");
                    std::process::exit(2);
                }
            };
        match ev {
            Ok(AgentEvent::Done { status, error, .. }) => {
                if let Some(error) = error {
                    eprintln!("ERR {error}");
                }
                break Some(status);
            }
            Ok(AgentEvent::TextDelta { text: delta }) => {
                text.push_str(&delta);
                eprintln!("TXT {delta}");
            }
            Ok(AgentEvent::ReasoningDelta { text }) => eprintln!("THK {}", text.trim_end()),
            Ok(AgentEvent::ToolCall { .. }) => {
                tools += 1;
                eprintln!("TOOL");
            }
            Ok(AgentEvent::Usage {
                input_tokens,
                output_tokens,
            }) => {
                eprintln!("USAGE {input_tokens}/{output_tokens}");
            }
            Ok(AgentEvent::Error { message }) => eprintln!("CHIP {message}"),
            Ok(other) => eprintln!("EV {other:?}"),
            Err(e) => eprintln!("ERR {e}"),
        }
    };
    eprintln!("--- done: {status:?} text={text:?} tools={tools}");
    // Let the adapter finish its teardown: the stream closes after it.
    let drained = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while stream.next().await.is_some() {}
    })
    .await;
    eprintln!(
        "--- stream closed after teardown: {} ({:?} since start)",
        drained.is_ok(),
        started.elapsed()
    );
    print_resolution();
    match status {
        Some(luna_proto::DoneStatus::Interrupted) if interrupt_after.is_some() => {}
        Some(luna_proto::DoneStatus::Completed) if !text.trim().is_empty() => {}
        _ => std::process::exit(1),
    }
}

/// The binary and API contract this process used (OC-00/OC-02 evidence).
fn print_resolution() {
    let Some(resolution) = luna_harness::opencode::last_opencode_resolution() else {
        eprintln!("RESOLVED none");
        return;
    };
    let version = resolution.version.as_deref().unwrap_or("unknown");
    eprintln!(
        "RESOLVED {} version={version} source={:?}",
        resolution.path.display(),
        resolution.source
    );
    if let Some(report) = luna_harness::opencode::opencode_compat_report(version) {
        eprintln!("COMPAT {report:?}");
    }
}
