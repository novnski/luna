//! Subprocess regressions for provider replies that never arrive.
//! Failure modes: Stop or a dropped stream during initialize, the first
//! turn/start, an idle follow-up turn/start, or turn/steer must reap the child.
//! A dispatched steer without an ACK stays unknown and must never be replayed.
#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

use futures::StreamExt;
use luna_harness::{
    CancellationToken, ClaudeHarness, CodexHarness, Harness, RunControls, SteerAcceptance,
    SteerDelivery, SteerMessage,
};
use luna_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel};
use tokio::sync::{mpsc, oneshot};

const PEER: &str = r#"#!/bin/sh
printf '%s' "$$" > "$CODEX_HOME/pid"
mode=$(cat "$CODEX_HOME/mode")
emit() { printf '%s\n' "$1"; }
rid() { printf '%s' "$1" | sed 's/.*"id":\([0-9]*\).*/\1/'; }
block() { printf '%s' "$mode" > "$CODEX_HOME/blocked"; exec sleep 30; }
read -r line || exit 1
[ "$mode" != initialize ] || block
emit "{\"id\":$(rid "$line"),\"result\":{}}"
read -r line || exit 1
read -r line || exit 1
emit "{\"id\":$(rid "$line"),\"result\":{\"thread\":{\"id\":\"audit-thread\"}}}"
read -r line || exit 1
case "$line" in *'"method":"thread/goal/get"'*)
  # Startup goal readback (installed 0.159 ThreadGoalGetResponse).
  emit "{\"id\":$(rid "$line"),\"result\":{\"goal\":null}}"
  read -r line || exit 1 ;;
esac
[ "$mode" != first ] || block
emit "{\"id\":$(rid "$line"),\"result\":{\"turn\":{\"id\":\"audit-turn\"}}}"
if [ "$mode" = followup ]; then
  emit '{"method":"turn/completed","params":{"threadId":"audit-thread","turn":{"id":"audit-turn","status":"completed"}}}'
fi
read -r line || exit 1
printf '%s\n' "$line" > "$CODEX_HOME/dispatch"
if [ "$mode" = lost-steer ]; then
  # The input was accepted, but only its reply channel failed. The queued
  # terminal notification must not authorize replay on the still-open stdin.
  emit '{"method":"turn/completed","params":{"threadId":"audit-thread","turn":{"id":"audit-turn","status":"completed"}}}'
  exec 1>&-
  while read -r line; do printf '%s\n' "$line" >> "$CODEX_HOME/replayed"; done
  exit 0
fi
block
"#;

const CLAUDE_PEER: &str = r#"#!/bin/sh
printf '%s' "$$" > pid
read -r line || exit 1
printf '%s\n' '{"type":"system","subtype":"init","model":"synthetic","tools":[],"session_id":"audit-claude"}'
# Read one byte of the large steering message, then stop consuming stdin.
# This proves dispatch began while ensuring the write cannot complete.
dd bs=1 count=1 of=/dev/null 2>/dev/null
printf '%s' blocked > blocked
exec sleep 30
"#;

async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture reached the requested protocol boundary");
}

async fn blocked_reply(mode: &str, drop_consumer: bool) {
    let home = tempfile::tempdir().unwrap();
    let exe = home.path().join("peer.sh");
    std::fs::write(&exe, if mode == "claude" { CLAUDE_PEER } else { PEER }).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(home.path().join("mode"), mode).unwrap();
    let harness: Box<dyn Harness> = if mode == "claude" {
        Box::new(
            ClaudeHarness::new()
                .with_executable(exe)
                .with_graces(Duration::from_millis(20), Duration::from_millis(20)),
        )
    } else {
        Box::new(
            CodexHarness::new()
                .with_executable(exe)
                .with_graces(Duration::from_millis(20), Duration::from_millis(20)),
        )
    };
    let interrupt = CancellationToken::new();
    let (steer, steering) = mpsc::channel(2);
    let controls = RunControls {
        interrupt: interrupt.clone(),
        steering,
        invoke_luna_tool: None,
        request_input: Box::new(|_| {
            let (_tx, rx) = oneshot::channel();
            rx
        }),
    };
    let mut request = RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: "disposable QA".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: home.path().to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![],
        worktree: None,
        resume: None,
    };
    request.model_options.insert(
        luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.path().to_string_lossy().into_owned().into(),
    );
    let mut stream = harness.run(request, controls).await.unwrap();
    let mut acceptance = None;
    if matches!(mode, "steer" | "followup" | "lost-steer" | "claude") {
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = stream.next().await {
                let ready = match event.unwrap() {
                    AgentEvent::ProviderCheckpoint { .. } => mode != "followup",
                    AgentEvent::SessionStarted { .. } => mode == "claude",
                    AgentEvent::Done {
                        status: DoneStatus::Completed,
                        ..
                    } => mode == "followup",
                    _ => false,
                };
                if ready {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let (tx, rx) = oneshot::channel();
        acceptance = Some(rx);
        steer
            .send(SteerMessage {
                input_items: Vec::new(),
                attachments: Vec::new(),
                prompt: if mode == "claude" {
                    "x".repeat(4 * 1024 * 1024)
                } else {
                    "redirect once".into()
                },
                message_id: None,
                delivery: if matches!(mode, "steer" | "claude") {
                    SteerDelivery::RequireActiveTurn
                } else {
                    SteerDelivery::AllowFollowUpTurn
                },
                acceptance: Some(tx),
            })
            .await
            .unwrap();
    }
    wait_file(&home.path().join(if mode == "lost-steer" {
        "dispatch"
    } else {
        "blocked"
    }))
    .await;
    let pid: i32 = std::fs::read_to_string(home.path().join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    let mut events = Vec::new();
    let settled = if drop_consumer {
        drop(stream);
        true
    } else {
        if mode != "lost-steer" {
            interrupt.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = stream.next().await {
                events.push(event.unwrap());
            }
        })
        .await;
        drop(stream);
        result.is_ok()
    };
    let reaped = tokio::time::timeout(Duration::from_secs(2), async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    if !reaped {
        // Bound even a failing-before-fix run to this exact disposable peer.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let replayed = std::fs::read_to_string(home.path().join("replayed")).unwrap_or_default();
    assert!(
        settled && reaped,
        "mode={mode} drop_consumer={drop_consumer}: settled={settled}, reaped={reaped}, replayed={replayed:?}, events={events:?}"
    );
    if !drop_consumer && mode != "lost-steer" {
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        ..
                    }
                ))
                .count(),
            1,
            "{events:?}"
        );
    }
    if matches!(mode, "steer" | "claude") {
        assert!(
            matches!(acceptance.unwrap().await, Ok(SteerAcceptance::Unknown(_))),
            "dispatch without ACK must remain unknown"
        );
    }
    if mode == "steer" {
        let dispatch = std::fs::read_to_string(home.path().join("dispatch")).unwrap();
        assert!(dispatch.contains("turn/steer"));
    }
    if mode == "lost-steer" {
        assert!(
            !home.path().join("replayed").exists(),
            "uncertain steer was dispatched a second time"
        );
    }
}

macro_rules! case {
    ($name:ident, $mode:literal, $drop:literal) => {
        #[tokio::test]
        async fn $name() {
            blocked_reply($mode, $drop).await;
        }
    };
}
case!(interrupt_during_initialize, "initialize", false);
case!(drop_during_initialize, "initialize", true);
case!(interrupt_during_first_turn_start, "first", false);
case!(drop_during_first_turn_start, "first", true);
case!(interrupt_during_followup_turn_start, "followup", false);
case!(drop_during_followup_turn_start, "followup", true);
case!(interrupt_during_steer, "steer", false);
case!(drop_during_steer, "steer", true);
case!(lost_ordinary_steer_reply_never_replays, "lost-steer", false);
case!(claude_interrupt_during_blocked_steer_write, "claude", false);
case!(claude_drop_during_blocked_steer_write, "claude", true);

// A malformed or incomplete successful JSON-RPC response must not replace a
// last-known-good account catalog. A genuine empty data array remains valid.
async fn model_page(page: &str) -> Result<Vec<luna_proto::Model>, luna_harness::HarnessError> {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("page"), page).unwrap();
    let exe = home.path().join("catalog.sh");
    std::fs::write(
        &exe,
        r#"#!/bin/sh
while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  case "$line" in
    *'"method":"model/list"'*) printf '{"id":%s,"result":%s}\n' "$id" "$(cat "$CODEX_HOME/page")" ;;
    *) printf '{"id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    CodexHarness::new()
        .with_executable(exe)
        .with_graces(Duration::from_millis(20), Duration::from_millis(20))
        .live_models_for_home(home.path(), false)
        .await
}

#[tokio::test]
async fn malformed_model_catalog_is_not_authoritative_empty() {
    for page in [
        "{}",
        r#"{"data":null}"#,
        r#"{"data":{},"nextCursor":null}"#,
        r#"{"data":[],"nextCursor":42}"#,
    ] {
        assert!(
            model_page(page).await.is_err(),
            "malformed page accepted: {page}"
        );
    }
    assert!(
        model_page(r#"{"data":[],"nextCursor":null}"#)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cyclic_model_pagination_is_not_authoritative_partial() {
    assert!(
        model_page(r#"{"data":[{"id":"visible"}],"nextCursor":"same-page"}"#)
            .await
            .is_err()
    );
}
