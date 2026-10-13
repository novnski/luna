//! Real Codex adapter subprocess policy receipts, without a model/account,
//! and the per-mode settlement of every approval Codex can send a client
//! (`docs/pending-tasks/permission-modes/README.md`).
#![cfg(unix)]

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use luna_harness::{CancellationToken, CodexHarness, Harness, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, PermissionMode, PermissionPolicy, RunRequest, SandboxLevel,
    UserInputAnswer, UserInputQuestion,
};
use serde_json::{Value, json};

fn peer() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let executable =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/permission-modes.py");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    executable
}

/// Whether one message contains every part.
fn any_has(messages: &[&str], parts: &[&str]) -> bool {
    messages
        .iter()
        .any(|message| parts.iter().all(|part| message.contains(part)))
}

/// Legacy Ask still parks every approval card; Full answers each one once
/// without a card or a saved rule; Auto declines whatever Codex's reviewer
/// handed back to a person, shows why, and shows the reviewer's own denial and
/// timeout. A form that asks for values reaches the user in every mode. The
/// peer (`fixtures/permission-modes.py`) replays Codex's frame shapes and
/// fails the turn on any response that differs from the mode's contract.
///
/// Regressions this catches: an Auto MCP tool, computer-use, server or command
/// approval surfacing a human card; Full prompting, or persisting a session or
/// always rule; an Auto escalation silently accepted, or declined with no
/// visible reason; a reviewer denial left invisible; legacy Ask no longer
/// asking; a data form auto-answered.
#[tokio::test]
async fn codex_auto_and_full_settle_every_approval_without_a_person_and_ask_still_asks() {
    for mode in [
        PermissionMode::Ask,
        PermissionMode::Auto,
        PermissionMode::Full,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let asked: Arc<Mutex<Vec<Vec<UserInputQuestion>>>> = Arc::default();
        let seen = asked.clone();
        let (_steer, steering) = tokio::sync::mpsc::channel(8);
        let controls = RunControls {
            // Approval cards are denied and forms declined; a field question
            // gets no answer (the form's own Decline settles it).
            request_input: Box::new(move |questions| {
                let answers = questions
                    .iter()
                    .filter_map(|question| {
                        let label = ["Deny", "Decline"]
                            .into_iter()
                            .find(|label| question.options.iter().any(|option| option == label))?;
                        Some(UserInputAnswer {
                            question_id: question.id.clone(),
                            labels: vec![label.into()],
                        })
                    })
                    .collect();
                seen.lock().unwrap().push(questions);
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(answers);
                rx
            }),
            steering,
            interrupt: CancellationToken::new(),
            invoke_luna_tool: None,
        };
        let request: RunRequest = serde_json::from_value(json!({
            "prompt": "approvals", "cwd": dir.path(),
            "sandbox": if mode == PermissionMode::Full { SandboxLevel::DangerFullAccess } else { SandboxLevel::WorkspaceWrite },
            "permissionMode": mode, "autoApprove": mode == PermissionMode::Full,
            "permissionPolicy": PermissionPolicy { requested_mode: mode, effective_mode: mode, machine_name: "fixture-machine".into(), ..Default::default() },
            "modelOptions": {
                luna_harness::codex::LUNA_CODEX_HOME_OPTION: dir.path().join("codex-home"),
                luna_proto::LUNA_RUNTIME_CONTRACT_OPTION: {
                    "version":1, "contentHash":"permission-modes-fixture", "environment":{}, "localEndpoints":[], "instructions":[], "tools":[], "omittedTools":[], "delivery":"codexMcpTools",
                    "context":{"appVersion":"test", "protocolVersion":1, "threadId":"luna-fixture-task", "environmentId":"fixture-env", "machineId":"fixture-machine", "machineName":"fixture-machine", "workspaceRoot":"/tmp", "harness":"codex", "executionRoute":"local", "grantedScopes":[]}
                }
            }
        }))
        .unwrap();
        let stream = CodexHarness::new()
            .with_executable(peer())
            .run(request, controls)
            .await
            .unwrap();
        let events: Vec<_> =
            tokio::time::timeout(std::time::Duration::from_secs(15), stream.collect())
                .await
                .unwrap();
        let events: Vec<AgentEvent> = events
            .into_iter()
            .map(|event| event.expect("stream event"))
            .collect();
        let peer_log = std::fs::read_to_string(dir.path().join("permission-modes-approvals.json"))
            .unwrap_or_default();
        if let Ok(path) = std::env::var("LUNA_PERMISSION_MODES_RECEIPT") {
            use std::io::Write;
            let mut output = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(output, "{}", json!({"mode": mode, "scenario": "approvals", "peer": serde_json::from_str::<Value>(&peer_log).unwrap_or(Value::Null), "questions": *asked.lock().unwrap(), "events": events})).unwrap();
        }
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                })
            ),
            "{mode:?}: the peer rejected a response: {events:?}\n{peer_log}"
        );
        let errors: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Error { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        let declined: Vec<&str> = errors
            .iter()
            .copied()
            .filter(|message| message.starts_with("Auto access declined"))
            .collect();
        let reviewer: Vec<&str> = errors
            .iter()
            .copied()
            .filter(|message| message.starts_with("Codex's automatic reviewer"))
            .collect();
        let asked = asked.lock().unwrap();
        let cards = asked
            .iter()
            .flatten()
            .filter(|question| question.approval.is_some())
            .count();
        let forms: Vec<&Vec<UserInputQuestion>> = asked
            .iter()
            .filter(|set| {
                set.iter()
                    .all(|question| question.mcp.is_some() && question.approval.is_none())
            })
            .collect();
        match mode {
            PermissionMode::Ask => {
                // Command, file, permissions, MCP tool and computer use: one
                // card each. The message-only server form and the data form
                // are MCP question sets.
                assert_eq!(cards, 5, "Ask: {asked:?}");
                assert_eq!(forms.len(), 2, "Ask: {asked:?}");
                assert!(declined.is_empty() && reviewer.is_empty(), "{errors:?}");
            }
            PermissionMode::Auto => {
                // No approval card; only the data form reaches the user.
                assert_eq!(cards, 0, "Auto: {asked:?}");
                assert_eq!(forms.len(), 1, "Auto: {asked:?}");
                assert!(forms[0].iter().any(|question| question.header == "name"));
                // One visible reason per declined request, each naming it.
                assert_eq!(declined.len(), 6, "{errors:?}");
                for subject in [
                    "the command `touch approved.txt`",
                    "a file change",
                    "a request for additional permissions",
                    "executor's execute call",
                    "an approval request from executor",
                    "computer use of Finder",
                ] {
                    let named = any_has(&declined, &[subject, "Full access"]);
                    assert!(named, "no visible reason for {subject}: {declined:?}");
                }
                // The reviewer's denial and timeout; its approval and abort
                // add nothing.
                assert_eq!(reviewer.len(), 2, "{errors:?}");
                let denied = any_has(&reviewer, &["rm -rf ~/Documents", "outside the task"]);
                let timed_out = any_has(&reviewer, &["timed out", "execute"]);
                assert!(denied && timed_out, "{reviewer:?}");
            }
            PermissionMode::Full => {
                // No approval card; only the data form reaches the user.
                assert_eq!(cards, 0, "Full: {asked:?}");
                assert_eq!(forms.len(), 1, "Full: {asked:?}");
                assert!(declined.is_empty() && reviewer.is_empty(), "{errors:?}");
            }
        }
    }
}

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
