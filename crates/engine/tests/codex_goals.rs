//! "Send as goal" end to end: UI task command -> engine -> real CodexHarness
//! JSON-RPC -> the fake Codex peer, whose goal frames mirror the installed
//! 0.159.0 app-server (evidence/goal-parity/probes). No installed agent,
//! account or model is involved.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::CodexHarness;
use luna_proto::{
    ChatConfig, CommandReceipt, CommandReceiptStatus, ContextControlState, GoalStartRequest,
    GoalStatus, HarnessId, PermissionMode, ReasoningLevel, RunRequest, SandboxLevel,
    TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};

fn fake_codex() -> PathBuf {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-codex.sh");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn assemble(root: &Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CodexHarness::new().with_executable(fake_codex())));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "goal-org", "goal-user"),
        Arc::new(registry),
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap()
}

fn assemble_goal_attach(root: &Path, executable: &Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CodexHarness::new().with_executable(executable)));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let native = luna_harness::codex::native::CodexNativeClient::new(&accounts.codex_home)
        .with_executable(executable);
    EngineCore::assemble_with_profile_and_injected_codex_metadata_client(
        EngineProfile::development(&root.join("data"), "goal-org", "goal-user"),
        Arc::new(registry),
        HarnessId::Codex,
        None,
        accounts,
        native,
    )
    .unwrap()
}

/// Metadata Resume after restart must attach an owned runtime, preserving the
/// goal rather than replacing it or fabricating another user message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_goal_resume_attaches_runtime_without_replacing_goal_or_user_input() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("fake-codex-goal-attach.py");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../harness/tests/fixtures/fake-codex-goal-attach.py"),
        &executable,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let task = uuid::Uuid::new_v4().to_string();
    let core = assemble_goal_attach(root.path(), &executable);
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&client, &core, &task, root.path().to_str().unwrap()).await;
    let named = execute(
        &client,
        "name-cold-goal",
        &task,
        Some(&snapshot(&client, &task).await),
        serde_json::json!({"kind":"rename","title":"Cold goal fixture"}),
    )
    .await;
    assert_eq!(named.status, CommandReceiptStatus::Applied, "{named:?}");
    send(
        &client,
        &task,
        "initial-human",
        request("establish runtime", false, root.path().to_str().unwrap()),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !entries(&core, &task)
            .iter()
            .any(|entry| text(entry).contains("INITIAL_GOAL_RUNTIME"))
            || core.sessions.turn_in_flight(&task)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = entries(&core, &task)
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .count();
    let binding = snapshot(&client, &task).await.snapshot.binding_generation;
    core.shutdown().await;
    drop(client);
    drop(core);
    let goal = serde_json::json!({"threadId":"cold-goal-thread","objective":"saved objective", "status":"paused",
        "tokenBudget":9000,"tokensUsed":321,"timeUsedSeconds":17,"createdAt":100,"updatedAt":200});
    std::fs::write(
        root.path().join("goal-state.json"),
        serde_json::json!({"goal":goal}).to_string(),
    )
    .unwrap();
    let core = assemble_goal_attach(root.path(), &executable);
    let client = luna_rpc::memory_client(core.rpc_service());
    let read = snapshot(&client, &task).await;
    let intent = serde_json::json!({"kind":"goal", "operation":{"action":"set","status":"active"}});
    let receipt = execute(&client, "cold-resume", &task, Some(&read), intent.clone()).await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    tokio::time::timeout(Duration::from_secs(3), async {
        while !core.sessions.turn_in_flight(&task)
            || !entries(&core, &task)
                .iter()
                .any(|entry| text(entry).contains("OWNED_GOAL_CONTINUATION"))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Resume must start an owned continuation after cold metadata readback");
    assert_eq!(
        snapshot(&client, &task).await.snapshot.binding_generation,
        binding
    );
    assert_eq!(
        entries(&core, &task)
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        before
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("goal-state.json")).unwrap())
            .unwrap();
    for key in [
        "objective",
        "tokenBudget",
        "tokensUsed",
        "timeUsedSeconds",
        "createdAt",
    ] {
        assert_eq!(saved["goal"][key], goal[key]);
    }
    let trace = std::fs::read_to_string(root.path().join("goal-trace.jsonl")).unwrap();
    let events = trace
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|row| row["method"] == "turn/start")
            .count(),
        1,
        "only the initial human turn"
    );
    assert!(
        !events
            .iter()
            .any(|row| row["method"] == "thread/goal/clear")
    );
    let replay = execute(&client, "cold-resume", &task, Some(&read), intent).await;
    assert_eq!(replay.command_id, receipt.command_id);
    let before_steer = entries(&core, &task)
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .count();
    send(
        &client,
        &task,
        "goal-attach-user-steer",
        request(
            "human input after attachment",
            false,
            root.path().to_str().unwrap(),
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entries(&core, &task)
            .iter()
            .any(|entry| text(entry).contains("STEERED_OWNED_GOAL"))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        let trace =
            std::fs::read_to_string(root.path().join("goal-trace.jsonl")).unwrap_or_default();
        let methods = trace
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .map(|row| format!("{} owned={}", row["method"], row["owned"]))
            .collect::<Vec<_>>();
        panic!("steer failed {error}: {methods:?}");
    });
    assert_eq!(
        entries(&core, &task)
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        before_steer + 1
    );
    write_evidence("cold-goal-attach", &core, &task);
    core.sessions.interrupt_for_stop(&task).await.unwrap();
    drop(client);
    core.shutdown().await;
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Codex,
        provider_account_id: None,
        model: Some("gpt-6.1-sol".into()),
        reasoning: Some(ReasoningLevel::Ultra),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

/// A registered native client is not proof that a resumed goal owns a turn.
/// Refusal and a successful resume response with no turn must remain unknown.
#[tokio::test]
async fn cold_goal_resume_refusal_and_missing_turn_are_not_applied() {
    for failure in ["rejectAttach", "noTurnOnAttach", "incompatibleRuntime"] {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("fake-codex-goal-attach.py");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/fake-codex-goal-attach.py"),
            &executable,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let core = assemble_goal_attach(root.path(), &executable);
        let client = luna_rpc::memory_client(core.rpc_service());
        let task = uuid::Uuid::new_v4().to_string();
        let cwd = root.path().to_str().unwrap();
        create_task(&client, &core, &task, cwd).await;
        execute(
            &client,
            "name-failed-goal",
            &task,
            Some(&snapshot(&client, &task).await),
            serde_json::json!({"kind":"rename","title":"Failed attachment fixture"}),
        )
        .await;
        send(
            &client,
            &task,
            "initial-human",
            request("establish runtime", false, cwd),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while core.sessions.turn_in_flight(&task)
                || !entries(&core, &task)
                    .iter()
                    .any(|entry| text(entry).contains("INITIAL_GOAL_RUNTIME"))
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        core.shutdown().await;
        drop(client);
        drop(core);
        std::fs::write(root.path().join("goal-state.json"), serde_json::json!({
                (failure): true,
                "gateMetadataSet": failure == "incompatibleRuntime",
                "noTurnOnAttach": failure != "rejectAttach",
            "goal": {"threadId":"cold-goal-thread","objective":"saved objective","status":"paused",
                "tokenBudget":9000,"tokensUsed":321,"timeUsedSeconds":17,"createdAt":100,"updatedAt":200}
        }).to_string()).unwrap();
        let core = assemble_goal_attach(root.path(), &executable);
        let client = luna_rpc::memory_client(core.rpc_service());
        let read = snapshot(&client, &task).await;
        let resume_client = luna_rpc::memory_client(core.rpc_service());
        let resume_task = task.clone();
        let resume = tokio::spawn(async move {
            execute(
                &resume_client,
                "failed-cold-resume",
                &resume_task,
                Some(&read),
                serde_json::json!({"kind":"goal","operation":{"action":"set","status":"active"}}),
            )
            .await
        });
        if failure == "incompatibleRuntime" {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !root.path().join("metadata-set-waiting").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("metadata Resume reaches its cold reply gate");
            let mut competing = request("authorized competing input", false, cwd);
            competing.model = Some("different-model".into());
            core.sessions
                .dispatch(
                    &task,
                    HarnessId::Codex,
                    competing,
                    Some("competing-input".into()),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while core.sessions.turn_in_flight(&task)
                    || entries(&core, &task)
                        .iter()
                        .filter(|entry| text(entry).contains("INITIAL_GOAL_RUNTIME"))
                        .count()
                        != 2
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the competing runtime stays parked and live");
            std::fs::write(root.path().join("metadata-set-release"), "release").unwrap();
        }
        let receipt = resume.await.unwrap();
        assert_ne!(
            receipt.status,
            CommandReceiptStatus::Applied,
            "{failure}: {receipt:?}"
        );
        assert_eq!(
            receipt.reason.as_ref().map(|reason| reason.code),
            Some(luna_proto::CommandReasonCode::ProviderAcceptanceUnknown),
            "{failure}: {receipt:?}"
        );
        assert_eq!(
            entries(&core, &task)
                .iter()
                .filter(|entry| entry.role == MessageRole::User)
                .count(),
            if failure == "incompatibleRuntime" {
                2
            } else {
                1
            }
        );
        if failure == "incompatibleRuntime" {
            assert!(
                receipt
                    .reason
                    .as_ref()
                    .unwrap()
                    .message
                    .contains("different runtime"),
                "{receipt:?}"
            );
            assert!(
                core.sessions.has_live_run(&task),
                "Resume must preserve the competing runtime"
            );
            execute(
                &client,
                "read-competing-goal",
                &task,
                Some(&snapshot(&client, &task).await),
                serde_json::json!({"kind":"goal","operation":{"action":"get"}}),
            )
            .await;
            assert!(
                control(&core, &task).goal.unwrap().continuation_ready,
                "live readback must not mask an attach refusal"
            );
        }
        assert!(
            !entries(&core, &task)
                .iter()
                .any(|entry| text(entry).contains("OWNED_GOAL_CONTINUATION"))
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("goal-state.json")).unwrap())
                .unwrap();
        assert_eq!(saved["goal"]["status"], "active");
        assert_eq!(saved["goal"]["tokensUsed"], 321);
        write_evidence(&format!("cold-goal-{failure}"), &core, &task);
        drop(client);
        core.shutdown().await;
    }
}

#[tokio::test]
async fn cold_goal_resume_accepts_completion_before_readiness_is_observed() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("fake-codex-goal-attach.py");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../harness/tests/fixtures/fake-codex-goal-attach.py"),
        &executable,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let core = assemble_goal_attach(root.path(), &executable);
    let client = luna_rpc::memory_client(core.rpc_service());
    let task = uuid::Uuid::new_v4().to_string();
    create_task(&client, &core, &task, root.path().to_str().unwrap()).await;
    let named = execute(
        &client,
        "name-fast-goal",
        &task,
        Some(&snapshot(&client, &task).await),
        serde_json::json!({"kind":"rename","title":"Fast goal fixture"}),
    )
    .await;
    assert_eq!(named.status, CommandReceiptStatus::Applied);
    send(
        &client,
        &task,
        "fast-initial-human",
        request(
            "establish fast runtime",
            false,
            root.path().to_str().unwrap(),
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while core.sessions.turn_in_flight(&task)
            || !entries(&core, &task)
                .iter()
                .any(|entry| text(entry).contains("INITIAL_GOAL_RUNTIME"))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    core.shutdown().await;
    drop(client);
    drop(core);
    let core = assemble_goal_attach(root.path(), &executable);
    let client = luna_rpc::memory_client(core.rpc_service());
    std::fs::write(root.path().join("goal-state.json"),serde_json::json!({"completeOnAttach":true,"goal":{
        "threadId":"cold-goal-thread","objective":"finish immediately","status":"paused","tokenBudget":9000,
        "tokensUsed":321,"timeUsedSeconds":17,"createdAt":100,"updatedAt":200,
    }}).to_string()).unwrap();
    let receipt = execute(
        &client,
        "fast-cold-resume",
        &task,
        Some(&snapshot(&client, &task).await),
        serde_json::json!({"kind":"goal","operation":{"action":"set","status":"active"}}),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    assert_eq!(
        entries(&core, &task)
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        1
    );
    assert_eq!(
        control(&core, &task).goal.unwrap().goal.unwrap().status,
        GoalStatus::Complete
    );
    write_evidence("fast-cold-goal-attach", &core, &task);
    drop(client);
    core.shutdown().await;
}

fn request(prompt: &str, goal: bool, cwd: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: goal.then(|| GoalStartRequest {
            objective: prompt.into(),
            token_budget: None,
        }),
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::Codex),
        provider_account_id: None,
        model: Some("gpt-6.1-sol".into()),
        reasoning: Some(ReasoningLevel::Ultra),
        model_options: Default::default(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

async fn snapshot(client: &RpcClient, task_id: &str) -> TaskCommandRead {
    client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap()
}

async fn execute(
    client: &RpcClient,
    command_id: &str,
    task_id: &str,
    snapshot: Option<&TaskCommandRead>,
    intent: serde_json::Value,
) -> CommandReceipt {
    let mut value = serde_json::json!({
        "commandId": command_id,
        "actorId": "test-user",
        "clientId": "test-client",
        "clientSessionId": "test-session",
        "taskId": task_id,
        "intent": intent,
    });
    if let Some(snapshot) = snapshot {
        value["expectedTaskRevision"] = snapshot.snapshot.task_revision.into();
        if let Some(generation) = snapshot.snapshot.binding_generation {
            value["expectedBindingGeneration"] = generation.into();
        }
    }
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, value)
        .await
        .unwrap()
}

async fn create_task(client: &RpcClient, core: &EngineCore, task_id: &str, cwd: &str) {
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": "goal-project",
                "deviceId": core.device_id, "path": cwd
            }),
        )
        .await
        .unwrap();
    let receipt = execute(
        client,
        &format!("create-{task_id}"),
        task_id,
        None,
        serde_json::json!({
            "kind": "createChat",
            "spaceId": "goal-project",
            "cwd": cwd,
            "config": config(),
        }),
    )
    .await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
}

async fn send(
    client: &RpcClient,
    task_id: &str,
    message_id: &str,
    request: RunRequest,
) -> CommandReceipt {
    let read = snapshot(client, task_id).await;
    execute(
        client,
        message_id,
        task_id,
        Some(&read),
        serde_json::json!({
            "kind": "session",
            "command": {"kind": "run", "request": request, "messageId": message_id},
        }),
    )
    .await
}

fn entries(core: &EngineCore, task_id: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(task_id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap_or_default()
}

fn text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn control(core: &EngineCore, task_id: &str) -> ContextControlState {
    core.doc_host.context_control(task_id).unwrap()
}

/// Wait for the goal to be marked achieved and the provider turn to settle.
async fn wait_for_achieved(
    core: &EngineCore,
    client: &RpcClient,
    task_id: &str,
    marks: usize,
    peer_home: &Path,
) {
    let settled = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let achieved = control(core, task_id).goal_marks.achieved.len() >= marks;
            if achieved
                && snapshot(client, task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let view = core.task_identity.view(task_id);
    let peer_log = std::fs::read_to_string(peer_home.join("goal-warm-skipped")).unwrap_or_default();
    assert!(
        settled.is_ok(),
        "goal never completed: {:?}\nbinding: {:?}\nstatus: {:?}\npeer skipped: {peer_log}\nentries: {:?}",
        control(core, task_id),
        view.as_ref().and_then(|view| view.current_binding.clone()),
        core.sessions.session_status(task_id),
        entries(core, task_id)
            .iter()
            .map(|entry| (entry.role, entry.id.clone(), text(entry)))
            .collect::<Vec<_>>()
    );
}

fn write_evidence(name: &str, core: &EngineCore, task_id: &str) {
    let Some(directory) = std::env::var_os("LUNA_GOAL_EVIDENCE_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let transcript: Vec<_> = entries(core, task_id)
        .iter()
        .map(|entry| serde_json::json!({"id": entry.id, "role": entry.role, "text": text(entry)}))
        .collect();
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "provider": "fake Codex peer modelled on installed 0.159.0 goal frames",
            "transcript": transcript,
            "contextControl": control(core, task_id),
        }))
        .unwrap(),
    )
    .unwrap();
}

/// A goal sent to a task with no live runtime starts the runtime the way a
/// send does, and Codex's own goal turn runs immediately. Before this fix the
/// goal was written by a cold metadata client and nothing ran.
#[tokio::test]
async fn goal_sent_to_a_cold_task_starts_working_and_is_marked_achieved() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    // Managed task identities are UUIDs; human ids stay legacy chats.
    let task_id = uuid::Uuid::new_v4().to_string();
    let task = task_id.as_str();
    create_task(&client, &core, task, "/tmp").await;
    let objective = "scenario:goal-start count to three";
    let receipt = send(
        &client,
        task,
        "goal-message",
        request(objective, true, "/tmp"),
    )
    .await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_for_achieved(
        &core,
        &client,
        task,
        1,
        &root.path().join("synthetic-codex"),
    )
    .await;

    let entries = entries(&core, task);
    let users: Vec<_> = entries
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .collect();
    assert_eq!(users.len(), 1, "the goal message appears exactly once");
    assert_eq!(users[0].id, "goal-message");
    assert_eq!(text(users[0]), objective);
    let state = control(&core, task);
    assert!(state.goal_marks.is_sent("goal-message"), "{state:?}");
    let goal = state.goal.and_then(|readback| readback.goal).unwrap();
    assert_eq!(goal.status, GoalStatus::Complete);
    assert_eq!(goal.objective, objective);
    let mark = &state.goal_marks.achieved[0];
    assert_eq!(mark.time_used_seconds, 14);
    let achieved = entries
        .iter()
        .find(|entry| entry.id == mark.entry_id)
        .expect("the achieved mark names a transcript entry");
    assert_eq!(achieved.role, MessageRole::Assistant);
    assert_eq!(text(achieved).trim_end(), "1\n2\n3");
    write_evidence("engine-cold-goal", &core, task);
}

/// A goal sent while the Codex runtime is warm runs Codex's clear+set on the
/// loaded connection; Codex starts the goal turn itself and the engine records
/// it as a new turn after the message, never as a steer or a second message.
#[tokio::test]
async fn goal_sent_to_a_warm_task_starts_on_the_loaded_runtime() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    // Managed task identities are UUIDs; human ids stay legacy chats.
    let task_id = uuid::Uuid::new_v4().to_string();
    let task = task_id.as_str();
    // Its own folder: the peer resumes the same native thread there.
    let workspace = root.path().join("goal-warm-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let cwd = workspace.to_str().unwrap();
    create_task(&client, &core, task, cwd).await;
    // The first turn's runtime contract predates the provider binding, so
    // the next send restarts the runtime by design (contract changed). The
    // runtime it leaves warm is the one a later goal must reuse.
    for (index, message) in ["first-message", "second-message"].into_iter().enumerate() {
        send(
            &client,
            task,
            message,
            request("scenario:goal-warm", false, cwd),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let replies = entries(&core, task)
                    .iter()
                    .filter(|entry| {
                        entry.role == MessageRole::Assistant && text(entry).contains("ready")
                    })
                    .count();
                if replies > index
                    && snapshot(&client, task)
                        .await
                        .snapshot
                        .active_turn_id
                        .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("turn settles with the runtime still warm");
    }
    assert!(core.sessions.session_status(task).is_some());

    let objective = "Ship the warm goal";
    let receipt = send(
        &client,
        task,
        "warm-goal-message",
        request(objective, true, cwd),
    )
    .await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_for_achieved(
        &core,
        &client,
        task,
        1,
        &root.path().join("synthetic-codex"),
    )
    .await;

    let entries = entries(&core, task);
    let goal_index = entries
        .iter()
        .position(|entry| entry.id == "warm-goal-message")
        .expect("goal message");
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.role == MessageRole::User)
            .count(),
        3
    );
    assert_eq!(text(&entries[goal_index]), objective);
    let state = control(&core, task);
    assert!(state.goal_marks.is_sent("warm-goal-message"));
    assert!(!state.goal_marks.is_sent("second-message"));
    let mark = &state.goal_marks.achieved[0];
    let achieved_index = entries
        .iter()
        .position(|entry| entry.id == mark.entry_id)
        .expect("achieved entry");
    assert!(
        achieved_index > goal_index,
        "the goal turn follows its message"
    );
    assert_eq!(text(&entries[achieved_index]).trim_end(), "warm goal done");
    write_evidence("engine-warm-goal", &core, task);
}
