//! Subprocess E2E through the actual Codex adapter; deterministic peer, not live-provider proof.
#![cfg(unix)]
use futures::StreamExt;
use luna_harness::{CancellationToken, CodexHarness, Harness, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, PermissionMode, RunRequest, SandboxLevel, UserInputAnswer,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

#[tokio::test]
async fn permission_detail_wire_choices_and_scope() {
    for (scenario, label) in [
        ("command:once", "Allow once"),
        ("command:deny", "Deny"),
        ("command:cancel", "Cancel turn"),
        ("command:once:stale-completion", "Allow once"),
        ("command:exec", "Allow proposed command rule"),
        ("command:network", "Apply proposed network rule 1"),
        ("command:forged", "Allow for session"),
        ("permission:turn", "Allow for turn"),
        ("permission:session", "Allow for session"),
        ("permission:deny", "Deny"),
        ("permission:turn:stale", "Allow for turn"),
        ("permission:turn:unknown", "Allow for turn"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let (_steer, steering) = mpsc::channel(8);
        let controls = RunControls {
            request_input: Box::new(move |questions| {
                *seen.lock().unwrap() = questions.clone();
                let (tx, rx) = oneshot::channel();
                tx.send(
                    questions
                        .into_iter()
                        .map(|q| UserInputAnswer {
                            question_id: q.id,
                            labels: vec![label.into()],
                        })
                        .collect(),
                )
                .unwrap();
                rx
            }),
            steering,
            interrupt: CancellationToken::new(),
            invoke_luna_tool: None,
        };
        let mut model_options = serde_json::Map::new();
        model_options.insert(
            luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
            home.path().to_string_lossy().to_string().into(),
        );
        model_options.insert(luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(), serde_json::json!({
            "version":1, "contentHash":"permission-fixture", "environment":{}, "localEndpoints":[], "instructions":[], "tools":[], "omittedTools":[], "delivery":"codexDynamicTools",
            "context":{"appVersion":"test", "protocolVersion":1, "threadId":"luna-fixture-task", "environmentId":"fixture-env", "machineId":"fixture-machine", "machineName":"fixture-machine", "workspaceRoot":"/tmp", "harness":"codex", "executionRoute":"local", "grantedScopes":[]}
        }));
        let request = RunRequest {
            prompt: scenario.into(),
            harness: Some(HarnessId::Codex),
            provider_account_id: None,
            model: None,
            reasoning: None,
            model_options,
            cwd: "/tmp".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            permission_mode: PermissionMode::Ask,
            parent_permission_ceiling: None,
            environment_permission_ceiling: None,
            permission_policy: None,
            attachments: vec![],
            worktree: None,
            resume: None,
        };
        let harness = CodexHarness::new().with_executable(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/permission-detail.py"),
        );
        let stream = harness.run(request, controls).await.unwrap();
        let events: Vec<_> =
            tokio::time::timeout(std::time::Duration::from_secs(10), stream.collect())
                .await
                .unwrap();
        let expected_status = if scenario == "command:cancel" {
            DoneStatus::Interrupted
        } else {
            DoneStatus::Completed
        };
        assert!(
            matches!(events.last(), Some(Ok(AgentEvent::Done {status, ..})) if *status == expected_status),
            "{scenario}: expected {expected_status:?}; {events:?}"
        );
        if scenario == "command:cancel" {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, Ok(AgentEvent::Done { .. })))
                    .count(),
                1,
                "native Cancel must settle exactly once: {events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, Ok(AgentEvent::Steered { .. }))),
                "native Cancel must not synthesize a follow-up: {events:?}"
            );
        }
        let questions = asked.lock().unwrap();
        if scenario.ends_with(":stale") || scenario.ends_with(":unknown") {
            assert!(questions.is_empty(), "{scenario}: {questions:?}");
        } else {
            let details = questions[0]
                .approval
                .as_ref()
                .unwrap()
                .details
                .as_ref()
                .unwrap();
            assert_eq!(details.cwd.as_deref(), Some("/tmp"));
            assert_eq!(details.environment_id.as_deref(), Some("fixture-env"));
            assert_eq!(
                details.provider_thread_id.as_deref(),
                Some("permission-thread")
            );
            if scenario.starts_with("command:") {
                assert_eq!(details.kind.as_deref(), Some("writeStdin"));
                assert_eq!(details.approval_id.as_deref(), Some("opaque-callback"));
                assert_eq!(details.network.as_ref().unwrap().host, "example.test");
                assert!(!questions[0].options.contains(&"Allow for session".into()));
            } else {
                assert_eq!(
                    details.permissions.as_ref().unwrap()["fileSystem"]["write"][0],
                    "/tmp/write"
                );
            }
        }
        println!("permission-detail scenario={scenario} exact-wire-response=passed");
    }
}

#[tokio::test]
async fn permission_detail_remote_resolution_closes_parked_answer() {
    let home = tempfile::tempdir().unwrap();
    let (asked_tx, mut asked_rx) = mpsc::unbounded_channel();
    let (_steer, steering) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            asked_tx.send((questions, tx)).unwrap();
            rx
        }),
        steering,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    let mut model_options = serde_json::Map::new();
    model_options.insert(
        luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.path().to_string_lossy().to_string().into(),
    );
    let request = RunRequest {
        prompt: "command:once:resolved".into(),
        harness: Some(HarnessId::Codex),
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options,
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![],
        worktree: None,
        resume: None,
    };
    let harness = CodexHarness::new().with_executable(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/permission-detail.py"),
    );
    let stream = harness.run(request, controls).await.unwrap();
    let collecting = tokio::spawn(async move { stream.collect::<Vec<_>>().await });
    let (questions, response) =
        tokio::time::timeout(std::time::Duration::from_secs(5), asked_rx.recv())
            .await
            .unwrap()
            .unwrap();
    let events = tokio::time::timeout(std::time::Duration::from_secs(10), collecting)
        .await
        .unwrap()
        .unwrap();
    assert!(
        response
            .send(vec![UserInputAnswer {
                question_id: questions[0].id.clone(),
                labels: vec!["Allow once".into()]
            }])
            .is_err(),
        "resolved callback must close its answer receiver"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        )),
        "{events:?}"
    );
}
