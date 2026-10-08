//! Live E2E adapter probe, driven by scripts/qa-claude-writer-guard.py.
//! The controller supplies an executable that permits only real `agents
//! --json` reads. Any turn-launch attempt is recorded and refused by that
//! wrapper, so even a broken guard cannot send a model prompt.

use std::path::PathBuf;
use std::time::Duration;

use luna_harness::{CancellationToken, ClaudeHarness, Harness, HarnessError, RunControls};
use luna_proto::{RunRequest, SandboxLevel};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 4,
        "usage: claude_writer_guard_probe <safe-wrapper> <cwd> <session-uuid> <config-home-or-dash>"
    );
    let mut harness = ClaudeHarness::new().with_executable(PathBuf::from(&args[0]));
    if args[3] != "-" {
        harness = harness.with_config_dir(&args[3]);
    }
    let view = harness.external_sessions().await;
    anyhow::ensure!(
        view.unavailable_reason.is_none(),
        "session listing unavailable"
    );
    anyhow::ensure!(
        view.sessions.iter().any(|row| {
            row.session_id.as_deref() == Some(args[2].as_str())
                && row.process_alive
                && !row.owned_by_luna
        }),
        "the expected external writer is not visible to this harness"
    );
    let request: RunRequest = serde_json::from_value(json!({
        "prompt": "LUNA_WRITER_GUARD_PROBE_MUST_NEVER_REACH_A_MODEL",
        "model": null,
        "reasoning": null,
        "cwd": args[1],
        "sandbox": SandboxLevel::ReadOnly,
        "resume": args[2],
    }))?;
    let (_steer, steering) = mpsc::channel(1);
    let interrupt = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(|_| {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering,
        interrupt: interrupt.clone(),
        invoke_luna_tool: None,
    };
    let result =
        tokio::time::timeout(Duration::from_secs(12), harness.run(request, controls)).await;
    let refused = matches!(
        &result,
        Ok(Err(HarnessError::RejectedBeforeAcceptance(reason)))
            if reason.starts_with("This Claude conversation is already open in ")
    );
    interrupt.cancel();
    drop(result);
    println!(
        "{}",
        json!({
            "probePid": std::process::id(),
            "sessionId": args[2],
            "configuredHome": args[3] != "-",
            "externalWriterVisible": true,
            "rejectedBeforeAcceptance": refused,
        })
    );
    anyhow::ensure!(refused, "cold resume was not rejected before acceptance");
    Ok(())
}
