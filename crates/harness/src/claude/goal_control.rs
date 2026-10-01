//! Claude goal controls outside a run (CC-14).
//!
//! - Read: the session transcript's own goal record. No process, no model.
//! - Clear: a one-shot `--resume` process that sends only Claude's local
//!   command `/goal clear` and reads its local reply (`result` with
//!   `local_command:"goal"`, no model turn), then reads the record back.
//!
//! The engine guarantees no warm runtime for the session is alive (one
//! writer). The control process uses LUNA's Ask launch flags whatever the
//! chat's mode: a local command needs no tool permission, and the control
//! path must never carry a bypass flag.

use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::process::{Command, Stdio};
use crate::{HarnessError, NativeGoalAction, NativeGoalControl, NativeGoalReadback};

use super::ClaudeHarness;
use super::goals::{CLEAR_LINE, LocalReply, parse_local_reply, read_transcript_goal};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn readback(
    harness: &ClaudeHarness,
    request: &NativeGoalControl,
) -> Option<luna_proto::ThreadGoal> {
    let dir = harness.goal_config_dir()?;
    let session = request.session_id.clone();
    let cwd = request.cwd.clone();
    tokio::task::spawn_blocking(move || read_transcript_goal(&dir, &session, &cwd))
        .await
        .ok()
        .flatten()
        .and_then(|goal| goal.to_thread_goal(&request.session_id))
}

pub(super) async fn run(
    harness: &ClaudeHarness,
    request: NativeGoalControl,
) -> Result<NativeGoalReadback, HarnessError> {
    if !valid_session_id(&request.session_id) {
        return Err(HarnessError::RejectedBeforeAcceptance(
            "this chat has no Claude session to read a goal from".into(),
        ));
    }
    if request.action == NativeGoalAction::Read {
        return Ok(NativeGoalReadback {
            goal: readback(harness, &request).await,
            reply: None,
        });
    }

    let exe = harness.resolve_executable()?;
    let mut cmd = Command::new(&exe);
    crate::compose_child_path(&mut cmd, &exe);
    cmd.args([
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-prompt-tool",
        "stdio",
    ]);
    cmd.args(super::profile::mode_args(luna_proto::PermissionMode::Ask));
    cmd.arg(format!("--resume={}", request.session_id));
    if let Some(dir) = &harness.config_home {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    if !request.cwd.is_empty() {
        cmd.current_dir(&request.cwd);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HarnessError::NotInstalled(exe.display().to_string())
        } else {
            HarnessError::Io(error)
        }
    })?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        crate::shutdown_child(&mut child, harness.kill_grace).await;
        return Err(HarnessError::RejectedBeforeAcceptance(
            "Claude control process has no stdio".into(),
        ));
    };
    let exchange = async {
        let line = super::wire::user_message_line(CLEAR_LINE);
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(HarnessError::Io)?;
        stdin.flush().await.map_err(HarnessError::Io)?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(HarnessError::Io)? {
            let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            match frame.get("type").and_then(Value::as_str) {
                // A local command asks for no tool; refuse anything that does.
                Some("control_request") => {
                    let id = frame
                        .get("request_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let reply = super::wire::control_response_line(
                        id,
                        serde_json::json!({
                            "behavior": "deny",
                            "message": "LUNA's goal control runs no tools",
                        }),
                    );
                    stdin
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .map_err(HarnessError::Io)?;
                    stdin.flush().await.map_err(HarnessError::Io)?;
                }
                Some("result") => return Ok(frame),
                _ => {}
            }
        }
        Err(HarnessError::Protocol(
            "Claude exited before answering /goal clear".into(),
        ))
    };
    let outcome = tokio::time::timeout(CONTROL_TIMEOUT, exchange).await;
    // End input so Claude exits on its own and finishes its session writes;
    // signal only if it does not.
    drop(stdin);
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        crate::shutdown_child(&mut child, harness.kill_grace).await;
    }
    let result = match outcome {
        Ok(result) => result?,
        Err(_) => {
            return Err(HarnessError::Protocol(
                "Claude did not answer /goal clear in time; the goal may or may not be cleared"
                    .into(),
            ));
        }
    };
    let text = result
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if result.get("local_command").and_then(Value::as_str) != Some("goal") {
        // A resume failure (unknown session) or anything that ran a model
        // turn is not Claude's local goal command.
        return Err(HarnessError::Protocol(format!(
            "Claude did not run /goal clear: {}",
            if text.is_empty() {
                "no local command reply"
            } else {
                &text
            }
        )));
    }
    match parse_local_reply(&text) {
        Some(LocalReply::Cleared(_) | LocalReply::NoGoal) => Ok(NativeGoalReadback {
            goal: readback(harness, &request).await,
            reply: Some(text),
        }),
        _ => Err(HarnessError::Protocol(format!(
            "Claude answered /goal clear unexpectedly: {text}"
        ))),
    }
}
