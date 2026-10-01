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

fn request(prompt: &str, goal: bool) -> RunRequest {
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
        cwd: "/tmp".into(),
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

async fn create_task(client: &RpcClient, core: &EngineCore, task_id: &str) {
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": "goal-project",
                "deviceId": core.device_id, "path": "/tmp"
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
            "cwd": "/tmp",
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
async fn wait_for_achieved(core: &EngineCore, client: &RpcClient, task_id: &str, marks: usize) {
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
    assert!(
        settled.is_ok(),
        "goal never completed: {:?}\nbinding: {:?}\nenvironment: {:?} device: {}\nentries: {:?}",
        control(core, task_id),
        view.as_ref().and_then(|view| view.current_binding.clone()),
        view.as_ref()
            .map(|view| view.workspace.environment_id.clone()),
        core.device_id,
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
    create_task(&client, &core, task).await;
    let objective = "scenario:goal-start count to three";
    let receipt = send(&client, task, "goal-message", request(objective, true)).await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_for_achieved(&core, &client, task, 1).await;

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
    create_task(&client, &core, task).await;
    send(
        &client,
        task,
        "first-message",
        request("scenario:goal-warm", false),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let replied = entries(&core, task)
                .iter()
                .any(|entry| entry.role == MessageRole::Assistant && text(entry).contains("ready"));
            if replied
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
    .expect("first turn settles with the runtime still warm");
    assert!(core.sessions.session_status(task).is_some());

    let objective = "Ship the warm goal";
    let receipt = send(&client, task, "warm-goal-message", request(objective, true)).await;
    assert_ne!(
        receipt.status,
        CommandReceiptStatus::Rejected,
        "{receipt:?}"
    );
    wait_for_achieved(&core, &client, task, 1).await;

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
        2
    );
    assert_eq!(text(&entries[goal_index]), objective);
    let state = control(&core, task);
    assert!(state.goal_marks.is_sent("warm-goal-message"));
    assert!(!state.goal_marks.is_sent("first-message"));
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
