//! Real Codex adapter subprocess policy receipts, without a model/account.
#![cfg(unix)]

use futures::StreamExt;
use luna_harness::{CancellationToken, CodexHarness, Harness, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, PermissionMode, PermissionPolicy, RunRequest, SandboxLevel,
};
use serde_json::{Value, json};

#[tokio::test]
async fn codex_start_resume_and_turn_keep_the_same_explicit_permission_policy() {
    use std::os::unix::fs::PermissionsExt;
    let executable =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/permission-modes.py");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    for resume in [false, true] {
        for (mode, approval, reviewer, sandbox, turn_sandbox) in [
            (
                PermissionMode::Ask,
                "on-request",
                "user",
                "workspace-write",
                "workspaceWrite",
            ),
            (
                PermissionMode::Auto,
                "on-request",
                "auto_review",
                "workspace-write",
                "workspaceWrite",
            ),
            (
                PermissionMode::Full,
                "never",
                "user",
                "danger-full-access",
                "dangerFullAccess",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (_steer, steering) = tokio::sync::mpsc::channel(8);
            let controls = RunControls {
                request_input: Box::new(|_| panic!("this fixture never requests approval")),
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: None,
            };
            let request: RunRequest = serde_json::from_value(json!({
                "prompt":"policy receipt", "cwd":dir.path(),
                "sandbox":if mode == PermissionMode::Full { SandboxLevel::DangerFullAccess } else { SandboxLevel::WorkspaceWrite },
                "permissionMode":mode, "autoApprove":mode == PermissionMode::Full,
                "permissionPolicy":PermissionPolicy { requested_mode:mode, effective_mode:mode, ..Default::default() },
                "resume":resume.then_some("permission-modes-thread"),
                "modelOptions":{luna_harness::codex::LUNA_CODEX_HOME_OPTION:dir.path().join("codex-home")}
            })).unwrap();
            let stream = CodexHarness::new()
                .with_executable(&executable)
                .run(request, controls)
                .await
                .unwrap();
            let events: Vec<_> =
                tokio::time::timeout(std::time::Duration::from_secs(10), stream.collect())
                    .await
                    .unwrap();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    Ok(AgentEvent::Done {
                        status: DoneStatus::Completed,
                        ..
                    })
                )),
                "{mode:?} resume={resume}: {events:?}"
            );
            let receipt =
                std::fs::read_to_string(dir.path().join("permission-modes-wire.jsonl")).unwrap();
            if let Ok(path) = std::env::var("LUNA_PERMISSION_MODES_RECEIPT") {
                use std::io::Write;
                let mut output = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap();
                writeln!(output, "{}", json!({"mode":mode,"resume":resume,"wire":receipt.lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>()})).unwrap();
            }
            let wire: Vec<Value> = receipt
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let thread = &wire
                .iter()
                .find(|row| {
                    row["method"]
                        == if resume {
                            "thread/resume"
                        } else {
                            "thread/start"
                        }
                })
                .unwrap()["params"];
            let turn = &wire
                .iter()
                .find(|row| row["method"] == "turn/start")
                .unwrap()["params"];
            for params in [thread, turn] {
                assert_eq!(params["approvalPolicy"], approval);
                assert_eq!(params["approvalsReviewer"], reviewer);
            }
            assert_eq!(thread["sandbox"], sandbox);
            assert_eq!(turn["sandboxPolicy"]["type"], turn_sandbox);
            if mode != PermissionMode::Full {
                assert_eq!(turn["sandboxPolicy"]["networkAccess"], true);
                assert_eq!(
                    thread["config"]["sandbox_workspace_write.network_access"], true,
                    "server-started goal/review turns must use the same network policy as ordinary turns"
                );
            }
        }
    }
}
