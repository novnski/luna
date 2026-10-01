//! Claude Code's own session supervisor, read-only: `claude agents --json`.
//!
//! Live 2.1.286 evidence (isolated config, no model turn):
//! - The listing does not start the supervisor daemon. `claude --bg` does, as
//!   a transient daemon that idle-exits.
//! - Background rows carry `id`, `state` and, while alive, `pid`/`status`.
//!   After `claude stop` they keep `state: "stopped"` with no `pid`.
//! - Every live print-mode process is listed too, as `kind: "interactive"`
//!   with its `sessionId`. That includes LUNA's own chat processes, which is
//!   why rows whose parent process is this engine are marked `owned_by_luna`.
//!
//! LUNA never attaches, stops, removes or respawns these sessions. The only
//! action it takes is refusing to resume a conversation that another live
//! process is already writing (two writers on one native transcript).

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use luna_proto::{
    ClaudeExternalSession, ClaudeExternalSessionActivity, ClaudeExternalSessionKind,
    ClaudeExternalSessionState, ClaudeExternalSessionsView,
};

use crate::HarnessError;
use crate::process::{Command, Stdio};

const MAX_ROWS: usize = 200;
const MAX_STDOUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_FIELD_CHARS: usize = 512;
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// The resume guard runs before every cold `--resume`; keep it short.
const GUARD_TIMEOUT: Duration = Duration::from_secs(3);

fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_FIELD_CHARS).collect())
}

fn parse_row(row: &Value) -> Option<(ClaudeExternalSession, Option<u32>)> {
    let object = row.as_object()?;
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some("interactive") => ClaudeExternalSessionKind::Interactive,
        Some("background") => ClaudeExternalSessionKind::Background,
        _ => ClaudeExternalSessionKind::Unknown,
    };
    let state = object
        .get("state")
        .and_then(Value::as_str)
        .map(|state| match state {
            "working" => ClaudeExternalSessionState::Working,
            "blocked" => ClaudeExternalSessionState::Blocked,
            "done" => ClaudeExternalSessionState::Done,
            "failed" => ClaudeExternalSessionState::Failed,
            "stopped" => ClaudeExternalSessionState::Stopped,
            _ => ClaudeExternalSessionState::Unknown,
        });
    let activity = object
        .get("status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "busy" => ClaudeExternalSessionActivity::Busy,
            "waiting" => ClaudeExternalSessionActivity::Waiting,
            "idle" => ClaudeExternalSessionActivity::Idle,
            _ => ClaudeExternalSessionActivity::Unknown,
        });
    let pid = object
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1);
    Some((
        ClaudeExternalSession {
            kind,
            short_id: text(object.get("id")),
            session_id: text(object.get("sessionId")),
            name: text(object.get("name")),
            cwd: text(object.get("cwd")).unwrap_or_default(),
            started_at_ms: object.get("startedAt").and_then(Value::as_u64),
            state,
            activity,
            waiting_for: text(object.get("waitingFor")),
            process_alive: pid.is_some(),
            owned_by_luna: false,
        },
        pid,
    ))
}

/// Parse `claude agents --json` stdout. `Err` carries a user-facing reason.
pub fn parse_listing(
    stdout: &[u8],
) -> Result<(Vec<(ClaudeExternalSession, Option<u32>)>, bool), String> {
    if stdout.len() > MAX_STDOUT_BYTES {
        return Err("The Claude session listing was too large to read safely.".into());
    }
    let value: Value = serde_json::from_slice(stdout)
        .map_err(|_| "Claude returned a session listing LUNA could not read.".to_owned())?;
    let rows = value
        .as_array()
        .ok_or_else(|| "Claude returned a session listing in an unexpected shape.".to_owned())?;
    let truncated = rows.len() > MAX_ROWS;
    Ok((
        rows.iter().take(MAX_ROWS).filter_map(parse_row).collect(),
        truncated,
    ))
}

/// Parent process ids for the given pids (Unix `ps`). Missing entries mean
/// "unknown", never "owned".
#[cfg(unix)]
async fn parent_pids(pids: &[u32]) -> HashMap<u32, u32> {
    let mut parents = HashMap::new();
    if pids.is_empty() {
        return parents;
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut cmd = tokio::process::Command::new("ps");
    cmd.args(["-o", "pid=,ppid=", "-p", &list])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(2), cmd.output()).await else {
        return parents;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        if let (Some(Ok(pid)), Some(Ok(ppid))) = (
            fields.next().map(str::parse::<u32>),
            fields.next().map(str::parse::<u32>),
        ) {
            parents.insert(pid, ppid);
        }
    }
    parents
}

#[cfg(not(unix))]
async fn parent_pids(_pids: &[u32]) -> HashMap<u32, u32> {
    HashMap::new()
}

/// List native sessions with the given Claude executable. Never errors: an
/// unreadable listing is an explicit `unavailable_reason`, not "no sessions".
pub async fn list_external_sessions(exe: &Path, budget: Duration) -> ClaudeExternalSessionsView {
    let checked_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let unavailable = |reason: String| ClaudeExternalSessionsView {
        checked_at_ms,
        unavailable_reason: Some(reason),
        ..Default::default()
    };
    let mut cmd = Command::new(exe);
    crate::compose_child_path(&mut cmd, exe);
    super::coordination::scrub_inherited_session_env(&mut cmd);
    cmd.args(["agents", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("The Claude Code CLI is not installed.".into());
        }
        Err(_) => return unavailable("The Claude Code CLI could not be started.".into()),
    };
    // Dropping the future on timeout drops the child, which kills it.
    let output = match tokio::time::timeout(budget, child.wait_with_output()).await {
        Err(_) => return unavailable("Claude did not list its sessions in time.".into()),
        Ok(Err(_)) => return unavailable("The Claude Code CLI could not be read.".into()),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        return unavailable(
            "This Claude Code version did not list background sessions (`claude agents --json` failed)."
                .into(),
        );
    }
    let (rows, truncated) = match parse_listing(&output.stdout) {
        Ok(parsed) => parsed,
        Err(reason) => return unavailable(reason),
    };
    let pids: Vec<u32> = rows.iter().filter_map(|(_, pid)| *pid).collect();
    let parents = parent_pids(&pids).await;
    let engine = std::process::id();
    let sessions = rows
        .into_iter()
        .map(|(mut session, pid)| {
            session.owned_by_luna = pid
                .and_then(|pid| parents.get(&pid))
                .is_some_and(|ppid| *ppid == engine);
            session
        })
        .collect();
    ClaudeExternalSessionsView {
        checked_at_ms,
        sessions,
        unavailable_reason: None,
        truncated,
    }
}

impl super::ClaudeHarness {
    /// Read-only listing through the same executable resolution as launches.
    pub async fn external_sessions(&self) -> ClaudeExternalSessionsView {
        match self.resolve_executable() {
            Ok(exe) => list_external_sessions(&exe, LIST_TIMEOUT).await,
            Err(_) => ClaudeExternalSessionsView {
                unavailable_reason: Some("The Claude Code CLI is not installed.".into()),
                ..Default::default()
            },
        }
    }
}

/// Refuse a cold `--resume` of a conversation another live Claude process is
/// writing. Fails open: when the listing is unavailable the native CLI stays
/// the final authority, and the attempt is logged.
pub(crate) async fn refuse_concurrent_writer(exe: &Path, resume: &str) -> Result<(), HarnessError> {
    let view = list_external_sessions(exe, GUARD_TIMEOUT).await;
    if let Some(reason) = &view.unavailable_reason {
        tracing::debug!(target: "luna_harness::claude", %reason, "two-writer guard skipped");
        return Ok(());
    }
    let Some(writer) = view.sessions.iter().find(|session| {
        session.process_alive
            && !session.owned_by_luna
            && session.session_id.as_deref() == Some(resume)
    }) else {
        return Ok(());
    };
    let handle = match (&writer.kind, &writer.short_id) {
        (ClaudeExternalSessionKind::Background, Some(id)) => {
            format!("background session {id}. Stop it with `claude stop {id}`, or detach it, then retry")
        }
        _ => "another Claude Code process (a terminal or IDE session). Close that session, then retry"
            .to_owned(),
    };
    Err(HarnessError::RejectedBeforeAcceptance(format!(
        "This Claude conversation is already open in {handle}. LUNA did not send your message, so the two processes cannot both write to it."
    )))
}
