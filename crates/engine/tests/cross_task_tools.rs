//! L25 local backend evidence using only explicit synthetic homes and rows.

use std::collections::HashSet;
use std::sync::Arc;

use luna_engine::cross_task_tools::{
    CrossTaskAuthority, CrossTaskControlGrant, CrossTaskError, CrossTaskTools,
};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::CodexHarness;
use luna_proto::{
    ACTIVITY_ENVELOPE_VERSION, ActivityEnvelope, ActivityKind, ActivityOutcome, ActivityRedaction,
    ActivitySourceRef, CrossTaskControlAction, CrossTaskControlRequest, CrossTaskListRequest,
    CrossTaskReadRequest, CrossTaskSendMode, CrossTaskSendRequest, CrossTaskWaitRequest,
    CrossTaskWaitTarget, HarnessId, RuntimeDelivery, Session, SessionStatus, TaskLineage,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn fake_codex_tool_uses_real_durable_queue_without_extra_approval() {
    use luna_proto::{AgentEvent, DoneStatus, PermissionMode, RunRequest, SandboxLevel};

    let root = TempDir::new().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-codex.sh");
    registry.register(Arc::new(CodexHarness::new().with_executable(fake)));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        registry,
        HarnessId::Codex,
        None,
        account_config(root.path()),
    )
    .unwrap();
    let _rpc = core.rpc_service();
    let project_a = root.path().join("project-a");
    let project_b = root.path().join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    for (id, path) in [("project-a", &project_a), ("project-b", &project_b)] {
        core.workspace
            .create_space(id, &core.device_id, path.to_str().unwrap(), None, false)
            .unwrap();
    }
    let source = uuid::Uuid::new_v4().to_string();
    let target = uuid::Uuid::new_v4().to_string();
    let foreign = uuid::Uuid::new_v4().to_string();
    for (id, project, path) in [
        (&source, "project-a", &project_a),
        (&target, "project-a", &project_a),
        (&foreign, "project-b", &project_b),
    ] {
        core.workspace
            .create_chat(id, Some(project), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                id,
                Some(project),
                None,
                path.to_str(),
                TaskLineage::default(),
            )
            .unwrap();
    }
    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::Codex,
            RunRequest {
                review: None,
                goal: None,
                input_items: Vec::new(),
                prompt: format!("scenario:l25-send:{target}:{foreign}"),
                harness: None,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: project_a.display().to_string(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: false,
                permission_mode: PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                attachments: vec![],
                worktree: None,
                resume: None,
            },
            Some("l25-user-prompt".into()),
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let approvals = 0;
        loop {
            match events.recv().await.unwrap().event {
                AgentEvent::InputRequested { .. } => {
                    panic!("Ordinary app coordination must not ask again")
                }
                AgentEvent::Done { status, .. } => break (approvals, status),
                _ => {}
            }
        }
    })
    .await
    .expect("synthetic provider turn settled");
    assert_eq!(result.0, 0);
    assert_eq!(result.1, DoneStatus::Completed);
    let receipt = core
        .doc_host
        .task_command_receipt("l25-send-1")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, luna_proto::CommandReceiptStatus::Applied);
    assert_eq!(receipt.task_id, target);
    assert!(receipt.committed_at_ms.is_some());
    let queued = core
        .doc_host
        .queued_message(&target, "l25-send-1")
        .unwrap()
        .unwrap();
    assert_eq!(queued.text, "Synthetic L25 queued follow-up");
    assert!(
        core.doc_host
            .task_command_receipt("l25-send-deny")
            .unwrap()
            .is_none()
    );
    assert!(
        core.doc_host
            .task_command_receipt("l25-wrong-target")
            .unwrap()
            .is_none()
    );
    assert!(
        core.doc_host
            .task_command_receipt("l25-foreign")
            .unwrap()
            .is_none()
    );
    core.shutdown().await;
    drop(_rpc);
    drop(core);
    let reopened =
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(
                &root.path().join("engine"),
                "synthetic-org",
                "synthetic-user",
            ),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            account_config(root.path()),
        )
        .unwrap();
    let persisted = reopened
        .doc_host
        .task_command_receipt("l25-send-1")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, luna_proto::CommandReceiptStatus::Applied);
    assert_eq!(persisted.event_sequence, receipt.event_sequence);
    assert_eq!(
        reopened
            .doc_host
            .queued_message(&target, "l25-send-1")
            .unwrap()
            .unwrap()
            .text,
        "Synthetic L25 queued follow-up"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn provider_send_rejects_archived_target_without_parking() {
    use luna_proto::{AgentEvent, DoneStatus, PermissionMode, RunRequest, SandboxLevel};
    use luna_rpc::methods;

    let root = TempDir::new().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/fake-codex.sh"),
        ),
    ));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        registry,
        HarnessId::Codex,
        None,
        account_config(root.path()),
    )
    .unwrap();
    let rpc = core.rpc_service();
    let client = luna_rpc::memory_client(rpc.clone());
    let project_path = root.path().join("project-a");
    std::fs::create_dir_all(&project_path).unwrap();
    core.workspace
        .create_space(
            "project-a",
            &core.device_id,
            project_path.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let source = uuid::Uuid::new_v4().to_string();
    let target = uuid::Uuid::new_v4().to_string();
    for id in [&source, &target] {
        core.workspace
            .create_chat(id, Some("project-a"), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                id,
                Some("project-a"),
                None,
                project_path.to_str(),
                TaskLineage::default(),
            )
            .unwrap();
    }
    let snapshot = core
        .doc_host
        .task_command_snapshot(&target)
        .unwrap()
        .snapshot;
    let changed: luna_proto::CommandReceipt = client
        .call_as(
            methods::EXECUTE_TASK_COMMAND,
            serde_json::json!({
                "commandId": "target-revision-change",
                "actorId": "synthetic-user",
                "clientId": "synthetic-test",
                "clientSessionId": "synthetic-test",
                "taskId": target,
                "expectedTaskRevision": snapshot.task_revision,
                "intent": { "kind": "setArchived", "archived": true }
            }),
        )
        .await
        .unwrap();
    assert_eq!(changed.status, luna_proto::CommandReceiptStatus::Applied);
    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::Codex,
            RunRequest {
                review: None,
                goal: None,
                input_items: Vec::new(),
                prompt: format!("scenario:l25-send-stale:{target}"),
                harness: None,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: project_path.display().to_string(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: false,
                permission_mode: PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                attachments: vec![],
                worktree: None,
                resume: None,
            },
            Some("l25-stale-prompt".into()),
        )
        .await
        .unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            match events.recv().await.unwrap().event {
                AgentEvent::InputRequested { .. } => {
                    panic!("Archived-target refusal must not ask for approval")
                }
                AgentEvent::Done { status, .. } => break status,
                _ => {}
            }
        }
    })
    .await
    .expect("stale target provider turn settled");
    assert_eq!(status, DoneStatus::Completed);
    assert!(
        core.doc_host
            .task_command_receipt("l25-stale-target")
            .unwrap()
            .is_none()
    );
    assert!(
        core.doc_host
            .queued_message(&target, "l25-stale-target")
            .unwrap()
            .is_none()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn approved_provider_send_sql_failure_has_no_queued_receipt_or_row() {
    use luna_proto::{AgentEvent, DoneStatus, PermissionMode, RunRequest, SandboxLevel};

    let root = TempDir::new().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/fake-codex.sh"),
        ),
    ));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        registry,
        HarnessId::Codex,
        None,
        account_config(root.path()),
    )
    .unwrap();
    let _rpc = core.rpc_service();
    let project_path = root.path().join("project-a");
    std::fs::create_dir_all(&project_path).unwrap();
    core.workspace
        .create_space(
            "project-a",
            &core.device_id,
            project_path.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let source = uuid::Uuid::new_v4().to_string();
    let target = uuid::Uuid::new_v4().to_string();
    for id in [&source, &target] {
        core.workspace
            .create_chat(id, Some("project-a"), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                id,
                Some("project-a"),
                None,
                project_path.to_str(),
                TaskLineage::default(),
            )
            .unwrap();
    }
    let db_path = root
        .path()
        .join("engine/orgs/synthetic-org/synthetic-user/docs.sqlite3");
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER fail_l25_send BEFORE UPDATE ON snapshots
                         WHEN OLD.doc_id='{target}'
                         BEGIN SELECT RAISE(ABORT, 'synthetic L25 storage failure'); END;"
    ))
    .unwrap();
    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::Codex,
            RunRequest {
                review: None,
                goal: None,
                input_items: Vec::new(),
                prompt: format!("scenario:l25-send-storage-fail:{target}"),
                harness: None,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: project_path.display().to_string(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: false,
                permission_mode: PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                attachments: vec![],
                worktree: None,
                resume: None,
            },
            Some("l25-storage-failure-prompt".into()),
        )
        .await
        .unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            match events.recv().await.unwrap().event {
                AgentEvent::InputRequested { .. } => {
                    panic!("Storage failure must not park an app approval")
                }
                AgentEvent::Done { status, .. } => break status,
                _ => {}
            }
        }
    })
    .await
    .expect("SQL failure turn settled");
    assert_eq!(status, DoneStatus::Completed);
    let receipt = core
        .doc_host
        .task_command_receipt("l25-storage-fail")
        .unwrap()
        .unwrap();
    assert_ne!(receipt.status, luna_proto::CommandReceiptStatus::Applied);
    assert!(
        core.doc_host
            .queued_message(&target, "l25-storage-fail")
            .unwrap()
            .is_none()
    );
    db.execute_batch("DROP TRIGGER fail_l25_send").unwrap();
    core.shutdown().await;
}

fn control_grant(caller: &str, request: &CrossTaskControlRequest) -> CrossTaskControlGrant {
    CrossTaskControlGrant {
        caller_task_id: caller.into(),
        request: request.clone(),
        expires_at_ms: chrono::Utc::now().timestamp_millis() + 30_000,
    }
}

fn account_config(root: &std::path::Path) -> AgentAccountsConfig {
    let config = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    config.disable_claude_keychain_fallback().unwrap();
    config
}

fn activity(task_id: &str) -> ActivityEnvelope {
    ActivityEnvelope {
        schema_version: ACTIVITY_ENVELOPE_VERSION,
        event_id: format!("synthetic:{task_id}"),
        environment_id: "synthetic-environment".into(),
        device_id: "synthetic-environment".into(),
        task_id: task_id.into(),
        turn_id: "synthetic-turn".into(),
        sequence: 1,
        timestamp_ms: 1,
        kind: ActivityKind::Answer,
        outcome: ActivityOutcome::Succeeded,
        harness: None,
        provider: None,
        model: None,
        reasoning: None,
        project_id: Some("project-a".into()),
        workspace_id: Some("synthetic-workspace".into()),
        tool: None,
        app: None,
        file: None,
        command: None,
        summary: "[synthetic redacted summary]".into(),
        redaction: ActivityRedaction::Heuristic,
        source: ActivitySourceRef {
            task_id: task_id.into(),
            message_id: "synthetic-message".into(),
            part_id: None,
            available_here: true,
            unavailable_reason: None,
        },
    }
}

#[tokio::test]
async fn local_backend_pages_only_visible_tasks_and_waits_on_status_rows() {
    let root = TempDir::new().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("synthetic-engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        account_config(root.path()),
    )
    .unwrap();
    let rpc = core.rpc_service();
    let device = core.device_id.clone();
    let path_a = root.path().join("project-a");
    let path_b = root.path().join("project-b");
    std::fs::create_dir_all(&path_a).unwrap();
    std::fs::create_dir_all(&path_b).unwrap();
    core.workspace
        .create_space(
            "project-a",
            &device,
            &path_a.display().to_string(),
            None,
            false,
        )
        .unwrap();
    core.workspace
        .create_space(
            "project-b",
            &device,
            &path_b.display().to_string(),
            None,
            false,
        )
        .unwrap();
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    let task_c = uuid::Uuid::new_v4().to_string();
    for (task, project, path) in [
        (&task_a, "project-a", &path_a),
        (&task_b, "project-b", &path_b),
        (&task_c, "project-a", &path_a),
    ] {
        core.workspace
            .create_chat(task, Some(project), None, None, None)
            .unwrap();
        core.task_identity
            .register_new_task(
                task,
                Some(project),
                None,
                path.to_str(),
                TaskLineage::default(),
            )
            .unwrap();
        core.workspace.rename_chat(task, project).unwrap();
    }
    core.store
        .replace_activity_events(&task_a, &[activity(&task_a)])
        .unwrap();
    let authority = CrossTaskAuthority {
        caller_task_id: task_a.clone(),
        local_environment_id: device.clone(),
        visible_project_ids: HashSet::from(["project-a".into()]),
        allow_projectless: false,
        granted_scopes: HashSet::from(["threads.read".into()]),
        explicit_user_request: false,
        delivery: RuntimeDelivery::CodexMcpTools,
    };
    let tools = CrossTaskTools::new(rpc);
    let (attached, omitted) = tools.catalog(&authority);
    let attached_names = attached
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(
        attached_names,
        HashSet::from([
            "get_workspace",
            "manage_workspace",
            "automation_update",
            "list_automations",
            "create_thread",
            "list_child_models",
            "list_threads",
            "read_thread",
            "wait_threads",
            "send_message_to_thread",
        ])
    );
    assert!(
        attached
            .iter()
            .all(|tool| tool.required_scopes == ["threads.read"])
    );
    assert!(
        !omitted
            .iter()
            .any(|tool| tool.name == "send_message_to_thread")
    );
    let mut projectless_authority = authority.clone();
    projectless_authority.visible_project_ids.clear();
    projectless_authority.allow_projectless = true;
    let (projectless_attached, projectless_omitted) = tools.catalog(&projectless_authority);
    assert!(
        !projectless_attached
            .iter()
            .any(|tool| tool.name == "send_message_to_thread")
    );
    assert!(
        projectless_omitted
            .iter()
            .any(|tool| tool.name == "send_message_to_thread")
    );
    assert!(omitted.iter().any(|tool| tool.name == "archive_thread"));
    let cancel = CancellationToken::new();
    let listed = tools
        .list(
            &authority,
            CrossTaskListRequest {
                project_id: None,
                text: None,
                cursor: None,
                limit: Some(20),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(listed.tasks.len(), 2);
    assert!(
        listed
            .tasks
            .iter()
            .all(|task| task.project_id.as_deref() == Some("project-a"))
    );
    let first = tools
        .list(
            &authority,
            CrossTaskListRequest {
                project_id: None,
                text: None,
                cursor: None,
                limit: Some(1),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert!(first.has_more);
    let second = tools
        .list(
            &authority,
            CrossTaskListRequest {
                project_id: None,
                text: None,
                cursor: first.next_cursor,
                limit: Some(1),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert!(!second.has_more);
    let ids = [
        first.tasks[0].task_id.as_str(),
        second.tasks[0].task_id.as_str(),
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    assert_eq!(ids, HashSet::from([task_a.as_str(), task_c.as_str()]));
    let read = tools
        .read(
            &authority,
            CrossTaskReadRequest {
                task_id: task_a.clone(),
                cursor: None,
                limit: Some(20),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(read.turns[0].summary, "[synthetic redacted summary]");
    assert!(read.content_untrusted);
    assert!(
        tools
            .read(
                &authority,
                CrossTaskReadRequest {
                    task_id: task_b,
                    cursor: None,
                    limit: Some(20),
                },
                &cancel
            )
            .await
            .is_err()
    );
    core.workspace.record_session(&Session {
        last_completed_turn: Some("synthetic-turn".into()),
        chat_id: task_a.clone(),
        device_id: device.clone(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: chrono::Utc::now(),
        recovery: None,
    });
    let waited = tools
        .wait(
            &authority,
            CrossTaskWaitRequest {
                targets: vec![CrossTaskWaitTarget {
                    task_id: task_a.clone(),
                    after_cursor: None,
                }],
                timeout_ms: Some(200),
            },
            &cancel,
        )
        .await
        .unwrap();
    let event = waited.event.unwrap();
    assert!(event.completed);
    let unchanged = tools
        .wait(
            &authority,
            CrossTaskWaitRequest {
                targets: vec![CrossTaskWaitTarget {
                    task_id: task_a.clone(),
                    after_cursor: Some(event.cursor),
                }],
                timeout_ms: Some(10),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert!(unchanged.timed_out);

    let mut control_authority = authority.clone();
    control_authority.explicit_user_request = true;
    control_authority
        .granted_scopes
        .insert("threads.manage".into());
    let new_task = uuid::Uuid::new_v4().to_string();
    let create = CrossTaskControlRequest {
        action: CrossTaskControlAction::Create,
        task_id: new_task.clone(),
        project_id: "project-a".into(),
        environment_id: device.clone(),
        command_id: "synthetic-create".into(),
        destination_project_id: None,
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
    };
    let created = tools
        .control(
            &control_authority,
            &control_grant(&task_a, &create),
            create.clone(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        created.command.status,
        luna_proto::CommandReceiptStatus::Applied
    );
    assert!(core.workspace.chat(&new_task).unwrap().is_some());
    let created_retry = tools
        .control(
            &control_authority,
            &control_grant(&task_a, &create),
            create,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(created_retry.command.command_id, created.command.command_id);

    let snapshot = core
        .doc_host
        .task_command_snapshot(&task_c)
        .unwrap()
        .snapshot;
    let archive = CrossTaskControlRequest {
        action: CrossTaskControlAction::Archive,
        task_id: task_c.clone(),
        project_id: "project-a".into(),
        environment_id: device.clone(),
        command_id: "synthetic-archive".into(),
        destination_project_id: None,
        expected_task_revision: Some(snapshot.task_revision),
        expected_binding_generation: snapshot.binding_generation,
        expected_turn_id: None,
    };
    let denied = tools
        .control(
            &authority,
            &control_grant(&task_a, &archive),
            archive.clone(),
            &cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(denied, CrossTaskError::Scope);
    let archived = tools
        .control(
            &control_authority,
            &control_grant(&task_a, &archive),
            archive.clone(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        archived.command.status,
        luna_proto::CommandReceiptStatus::Applied
    );
    assert!(core.workspace.chat(&task_c).unwrap().unwrap().archived);
    let retried = tools
        .control(
            &control_authority,
            &control_grant(&task_a, &archive),
            archive,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(retried.command.command_id, archived.command.command_id);

    let snapshot = core
        .doc_host
        .task_command_snapshot(&task_c)
        .unwrap()
        .snapshot;
    let unarchive = CrossTaskControlRequest {
        action: CrossTaskControlAction::Unarchive,
        task_id: task_c.clone(),
        project_id: "project-a".into(),
        environment_id: device.clone(),
        command_id: "synthetic-unarchive".into(),
        destination_project_id: None,
        expected_task_revision: Some(snapshot.task_revision),
        expected_binding_generation: snapshot.binding_generation,
        expected_turn_id: None,
    };
    let unarchived = tools
        .control(
            &control_authority,
            &control_grant(&task_a, &unarchive),
            unarchive,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        unarchived.command.status,
        luna_proto::CommandReceiptStatus::Applied
    );
    assert!(!core.workspace.chat(&task_c).unwrap().unwrap().archived);

    let mut move_authority = control_authority.clone();
    move_authority
        .visible_project_ids
        .insert("project-b".into());
    let snapshot = core
        .doc_host
        .task_command_snapshot(&task_c)
        .unwrap()
        .snapshot;
    let move_request = CrossTaskControlRequest {
        action: CrossTaskControlAction::Move,
        task_id: task_c.clone(),
        project_id: "project-a".into(),
        environment_id: device.clone(),
        command_id: "synthetic-move".into(),
        destination_project_id: Some("project-b".into()),
        expected_task_revision: Some(snapshot.task_revision),
        expected_binding_generation: snapshot.binding_generation,
        expected_turn_id: None,
    };
    let move_result = tools
        .control(
            &move_authority,
            &control_grant(&task_a, &move_request),
            move_request,
            &cancel,
        )
        .await
        .unwrap();
    assert_ne!(
        move_result.command.status,
        luna_proto::CommandReceiptStatus::Applied
    );
    assert_eq!(
        core.workspace
            .chat(&task_c)
            .unwrap()
            .unwrap()
            .space_id
            .as_deref(),
        Some("project-a")
    );

    let snapshot = core
        .doc_host
        .task_command_snapshot(&task_c)
        .unwrap()
        .snapshot;
    let interrupt = CrossTaskControlRequest {
        action: CrossTaskControlAction::Interrupt,
        task_id: task_c.clone(),
        project_id: "project-a".into(),
        environment_id: device.clone(),
        command_id: "synthetic-stop".into(),
        destination_project_id: None,
        expected_task_revision: Some(snapshot.task_revision),
        expected_binding_generation: snapshot.binding_generation,
        expected_turn_id: Some("absent-turn".into()),
    };
    let stopped = tools
        .control(
            &control_authority,
            &control_grant(&task_a, &interrupt),
            interrupt,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        stopped.command.status,
        luna_proto::CommandReceiptStatus::Conflict
    );

    let mut send_authority = authority;
    send_authority.explicit_user_request = true;
    send_authority.granted_scopes.insert("threads.send".into());
    let send_request = CrossTaskSendRequest {
        task_id: task_a,
        message: "synthetic queued follow-up".into(),
        mode: CrossTaskSendMode::Queue,
        idempotency_key: "synthetic-command-1".into(),
    };
    let queued = tools
        .send(&send_authority, send_request.clone(), &cancel)
        .await
        .unwrap();
    assert_eq!(queued.queued_message_id, "synthetic-command-1");
    assert_eq!(
        queued.command.status,
        luna_proto::CommandReceiptStatus::Applied
    );
    assert_eq!(queued.delivery_state, "queued");
    let repeated = tools
        .send(&send_authority, send_request.clone(), &cancel)
        .await
        .unwrap();
    assert_eq!(repeated.command.command_id, queued.command.command_id);
    let conflict = tools
        .send(
            &send_authority,
            CrossTaskSendRequest {
                message: "different synthetic payload".into(),
                ..send_request
            },
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        conflict,
        CrossTaskError::CommandOutcome {
            status: luna_proto::CommandReceiptStatus::Conflict,
            ..
        }
    ));
}

/// One live endpoint published by the `mcp-hold` fake Codex scenario.
struct HeldEndpoint {
    url: String,
    token: String,
    thread: String,
    turn: String,
}

async fn held_endpoint(
    hold_dir: &std::path::Path,
    task: &str,
    events: &mut tokio::sync::broadcast::Receiver<luna_engine::JournaledEvent>,
) -> HeldEndpoint {
    let file = hold_dir.join(task);
    let mut seen = Vec::new();
    for _ in 0..600 {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
            if lines.len() == 4 {
                return HeldEndpoint {
                    url: lines[0].clone(),
                    token: lines[1].clone(),
                    thread: lines[2].clone(),
                    turn: lines[3].clone(),
                };
            }
        }
        while let Ok(event) = events.try_recv() {
            seen.push(format!("{:?}", event.event));
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("fake Codex never published its LUNA MCP endpoint for {task}; events: {seen:#?}");
}

/// What Codex 0.159 sends to an MCP server for one tool call.
fn mcp_tool_call(
    id: u64,
    name: &str,
    arguments: serde_json::Value,
    thread: &str,
    turn: &str,
    call_id: &str,
) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":name,"arguments":arguments,
        "_meta":{"threadId":thread,"callId":call_id,"sessionId":thread,
            "x-codex-turn-metadata":{"turn_id":turn,"thread_id":thread}}
    }})
}

async fn mcp_post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let response = client
        .post(url)
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .json(body)
        .send()
        .await
        .expect("loopback MCP endpoint reachable");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

fn tool_text(reply: &serde_json::Value) -> (bool, String) {
    (
        reply["result"]["isError"].as_bool().expect("tool result"),
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// E2E over the real per-process endpoint: the test is the MCP client the way
/// Codex is, against the engine, the Codex adapter and the durable backend.
#[tokio::test]
async fn luna_mcp_endpoint_enforces_identity_fences_and_parks_approval() {
    use luna_proto::{ApprovalDecision, ChatConfig, PermissionMode, RunRequest, SandboxLevel};

    let root = TempDir::new().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/fake-codex.sh"),
        ),
    ));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        registry,
        HarnessId::Codex,
        None,
        account_config(root.path()),
    )
    .unwrap();
    let _rpc = core.rpc_service();
    let project_a = root.path().join("project-a");
    let project_b = root.path().join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    for (id, path) in [("project-a", &project_a), ("project-b", &project_b)] {
        core.workspace
            .create_space(id, &core.device_id, path.to_str().unwrap(), None, false)
            .unwrap();
    }
    // The fake publishes each run's endpoint under the run's cwd.
    let hold_dir = project_a.join(".luna-mcp-hold");
    // Concurrent held chats need distinct provider threads (one binding each).
    std::fs::write(project_a.join(".luna-fake-thread-per-task"), b"").unwrap();
    let config = ChatConfig {
        harness: HarnessId::Codex,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::ReadOnly,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    };
    let new_task = |project: &str, path: &std::path::Path, parent: Option<&str>| {
        let id = uuid::Uuid::new_v4().to_string();
        core.workspace
            .create_chat(&id, Some(project), None, Some(config.clone()), None)
            .unwrap();
        core.task_identity
            .register_new_task(
                &id,
                Some(project),
                None,
                path.to_str(),
                TaskLineage {
                    parent_task_id: parent.map(str::to_owned),
                    ..Default::default()
                },
            )
            .unwrap();
        id
    };
    let source = new_task("project-a", &project_a, None);
    let other = new_task("project-a", &project_a, None);
    let sibling = new_task("project-a", &project_a, None);
    let foreign = new_task("project-b", &project_b, None);
    // Four levels: a child of `deep4` would be the forbidden fifth level.
    let deep1 = new_task("project-a", &project_a, None);
    let deep2 = new_task("project-a", &project_a, Some(&deep1));
    let deep3 = new_task("project-a", &project_a, Some(&deep2));
    let deep4 = new_task("project-a", &project_a, Some(&deep3));

    let hold = |task: String| {
        let core = &core;
        let cwd = project_a.display().to_string();
        async move {
            core.sessions
                .dispatch(
                    &task,
                    HarnessId::Codex,
                    RunRequest {
                        review: None,
                        goal: None,
                        input_items: Vec::new(),
                        prompt: "scenario:mcp-hold".into(),
                        harness: None,
                        provider_account_id: None,
                        model: None,
                        reasoning: None,
                        model_options: Default::default(),
                        cwd,
                        sandbox: SandboxLevel::ReadOnly,
                        auto_approve: false,
                        permission_mode: PermissionMode::Ask,
                        parent_permission_ceiling: None,
                        environment_permission_ceiling: None,
                        permission_policy: None,
                        attachments: vec![],
                        worktree: None,
                        resume: None,
                    },
                    Some(format!("mcp-hold-{task}")),
                )
                .await
                .unwrap();
        }
    };
    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    hold(source.clone()).await;
    let a = held_endpoint(&hold_dir, &source, &mut events).await;
    let http = reqwest::Client::new();
    assert!(a.url.starts_with("http://127.0.0.1:") && a.url.ends_with("/mcp"));

    // Identity: token, Origin, Host, method and path checks happen before any
    // JSON-RPC reaches the session loop.
    let list = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let status = http.post(&a.url).json(&list).send().await.unwrap().status();
    assert_eq!(status.as_u16(), 401, "missing bearer token");
    assert_eq!(mcp_post(&http, &a.url, "wrong-token", &list).await.0, 401);
    let forged_origin = http
        .post(&a.url)
        .bearer_auth(&a.token)
        .header("origin", "https://attacker.example")
        .json(&list)
        .send()
        .await
        .unwrap();
    assert_eq!(forged_origin.status().as_u16(), 403);
    let rebinding = http
        .post(&a.url)
        .bearer_auth(&a.token)
        .header("host", "attacker.example")
        .json(&list)
        .send()
        .await
        .unwrap();
    assert_eq!(rebinding.status().as_u16(), 403);
    let sse = http.get(&a.url).bearer_auth(&a.token).send().await.unwrap();
    assert_eq!(sse.status().as_u16(), 405);
    let wrong_path = http
        .post(a.url.replace("/mcp", "/other"))
        .bearer_auth(&a.token)
        .json(&list)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_path.status().as_u16(), 404);
    let oversized = http
        .post(&a.url)
        .bearer_auth(&a.token)
        .body(vec![b' '; (1 << 20) + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status().as_u16(), 413);
    let batch = serde_json::json!([list.clone()]);
    assert_eq!(
        mcp_post(&http, &a.url, &a.token, &batch).await.1["error"]["code"],
        -32600
    );

    // Codex's handshake and catalog: reads are marked read-only; question
    // cards, workspace mutations and message delivery are writes.
    let (status, init) = mcp_post(
        &http,
        &a.url,
        &a.token,
        &serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"codex-mcp-client","version":"0.159.0"}}}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "luna");
    let (_, listed) = mcp_post(&http, &a.url, &a.token, &list).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<HashSet<_>>();
    for name in [
        "luna_status",
        "list_threads",
        "read_thread",
        "wait_threads",
        "send_message_to_thread",
        "create_thread",
        "ask_user",
    ] {
        assert!(names.contains(name), "{name} missing from {names:?}");
    }
    for tool in tools {
        // Board tools carry their own annotations (board_tools_e2e.rs).
        if luna_proto::BOARD_TOOL_NAMES.contains(&tool["name"].as_str().unwrap_or_default()) {
            continue;
        }
        let read_only = !matches!(
            tool["name"].as_str(),
            Some(
                "send_message_to_thread"
                    | "create_thread"
                    | "automation_update"
                    | "manage_workspace"
                    | "ask_user"
            )
        );
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{tool}");
    }

    // Thread, turn and call-ID fence.
    let call = |id, name, args, thread: &str, turn: &str, call_id: &str| {
        mcp_tool_call(id, name, args, thread, turn, call_id)
    };
    let empty = serde_json::json!({});
    let (failed, text) = tool_text(
        &mcp_post(
            &http,
            &a.url,
            &a.token,
            &call(
                10,
                "luna_status",
                empty.clone(),
                "foreign-thread",
                &a.turn,
                "c-foreign",
            ),
        )
        .await
        .1,
    );
    assert!(failed && text.contains("not attached"), "{text}");
    let (failed, text) = tool_text(
        &mcp_post(
            &http,
            &a.url,
            &a.token,
            &call(
                11,
                "luna_status",
                empty.clone(),
                &a.thread,
                "t-stale",
                "c-stale",
            ),
        )
        .await
        .1,
    );
    assert!(failed && text.contains("not attached"), "{text}");
    let (failed, text) = tool_text(
        &mcp_post(
            &http,
            &a.url,
            &a.token,
            &call(12, "luna_status", empty.clone(), &a.thread, &a.turn, ""),
        )
        .await
        .1,
    );
    assert!(failed && text.contains("call ID"), "{text}");
    let status_call = call(
        13,
        "luna_status",
        empty.clone(),
        &a.thread,
        &a.turn,
        "c-status",
    );
    let (failed, text) = tool_text(&mcp_post(&http, &a.url, &a.token, &status_call).await.1);
    assert!(!failed, "{text}");
    assert!(text.contains(&source), "{text}");
    let (failed, text) = tool_text(&mcp_post(&http, &a.url, &a.token, &status_call).await.1);
    assert!(
        failed && text.contains("repeated"),
        "replayed call ID: {text}"
    );

    // Ownership and project scope come from the engine-built authority.
    let (failed, text) = tool_text(
        &mcp_post(
            &http,
            &a.url,
            &a.token,
            &call(
                14,
                "list_threads",
                serde_json::json!({"limit": 20}),
                &a.thread,
                &a.turn,
                "c-list",
            ),
        )
        .await
        .1,
    );
    assert!(!failed, "{text}");
    assert!(
        text.contains(&sibling) && !text.contains(&foreign),
        "{text}"
    );
    let (failed, _) = tool_text(
        &mcp_post(
            &http,
            &a.url,
            &a.token,
            &call(
                15,
                "read_thread",
                serde_json::json!({"taskId": foreign}),
                &a.thread,
                &a.turn,
                "c-read-foreign",
            ),
        )
        .await
        .1,
    );
    assert!(failed, "foreign-project read must be rejected");

    // Ordinary creation is an app action and completes without another card.
    let create_args = serde_json::json!({
        "prompt": "MCP child brief", "idempotencyKey": "mcp-child-1", "title": "MCP child"
    });
    let pending_create = {
        let http = http.clone();
        let body = call(
            20,
            "create_thread",
            create_args,
            &a.thread,
            &a.turn,
            "c-create-1",
        );
        let (url, token) = (a.url.clone(), a.token.clone());
        tokio::spawn(async move { mcp_post(&http, &url, &token, &body).await })
    };
    let (failed, text) = tool_text(&pending_create.await.unwrap().1);
    assert!(!failed, "{text}");
    let receipt: serde_json::Value = serde_json::from_str(&text).unwrap();
    let child = receipt["taskId"].as_str().unwrap().to_owned();
    assert_eq!(receipt["parentTaskId"], source.as_str());
    assert_eq!(
        core.task_identity
            .view(&child)
            .unwrap()
            .task
            .lineage
            .parent_task_id
            .as_deref(),
        Some(source.as_str())
    );

    // Exercise new workspace operations through the actual attached MCP
    // endpoint while the parent provider turn remains active.
    macro_rules! workspace_call {
        ($wire:expr,$call:expr,$name:expr,$arguments:expr) => {{
            let response = mcp_post(
                &http,
                &a.url,
                &a.token,
                &call($wire, $name, $arguments, &a.thread, &a.turn, $call),
            )
            .await
            .1;
            let (failed, text) = tool_text(&response);
            assert!(!failed, "{}: {text}", $call);
            serde_json::from_str::<serde_json::Value>(&text).unwrap()
        }};
    }
    let workspace = workspace_call!(21, "workspace-read", "get_workspace", serde_json::json!({}));
    assert_eq!(workspace["mainChatId"], source);
    assert_eq!(workspace["layout"]["root"]["activeChatId"], source);
    let split_request = serde_json::json!({"action":"openTab","taskIds":[child],"idempotencyKey":"runtime-split",
        "expectedLayoutRevision":workspace["layout"]["revision"],"destination":{"kind":"split","nodeId":workspace["layout"]["root"]["id"],
        "edge":"right","newPaneId":uuid::Uuid::new_v4(),"splitId":uuid::Uuid::new_v4()}});
    let split = workspace_call!(
        22,
        "workspace-split",
        "manage_workspace",
        split_request.clone()
    );
    assert_eq!(split["snapshot"]["root"]["kind"], "split");
    assert_eq!(
        split["snapshot"]["focusedPaneId"],
        workspace["layout"]["focusedPaneId"]
    );
    let replay = workspace_call!(
        23,
        "workspace-split-replay",
        "manage_workspace",
        split_request
    );
    assert_eq!(replay["snapshot"], split["snapshot"]);
    let stale = call(
        24,
        "manage_workspace",
        serde_json::json!({"action":"closeTabs","taskIds":[child],"idempotencyKey":"runtime-stale",
        "expectedLayoutRevision":workspace["layout"]["revision"]}),
        &a.thread,
        &a.turn,
        "workspace-stale",
    );
    let (failed, text) = tool_text(&mcp_post(&http, &a.url, &a.token, &stale).await.1);
    assert!(
        failed && text.starts_with("Layout changed since revision"),
        "{text}"
    );
    let closed = workspace_call!(
        25,
        "workspace-close",
        "manage_workspace",
        serde_json::json!({"action":"closeTabs","taskIds":[child],"idempotencyKey":"runtime-close",
        "expectedLayoutRevision":split["snapshot"]["revision"]})
    );
    let closed_layout: luna_proto::ChatWorkbenchSnapshot =
        serde_json::from_value(closed["snapshot"].clone()).unwrap();
    assert!(!closed_layout.contains_chat(&child));
    assert!(!core.workspace.chat(&child).unwrap().unwrap().archived);
    let fork = workspace_call!(
        26,
        "workspace-fork",
        "manage_workspace",
        serde_json::json!({"action":"forkThread","taskIds":[source],"idempotencyKey":"runtime-fork",
        "expectedLayoutRevision":closed["snapshot"]["revision"]})
    );
    let fork_id = fork["taskId"].as_str().unwrap().to_owned();
    assert_eq!(fork["sourceTaskId"], source);
    assert_eq!(fork["startupState"], "idle");
    assert_eq!(
        core.task_identity
            .view(&fork_id)
            .unwrap()
            .task
            .lineage
            .source_task_id
            .as_deref(),
        Some(source.as_str())
    );
    let fork2 = workspace_call!(
        27,
        "workspace-fork-two",
        "manage_workspace",
        serde_json::json!({"action":"forkThread","taskIds":[source],"idempotencyKey":"runtime-fork-two",
        "expectedLayoutRevision":fork["placement"]["snapshot"]["revision"]})
    );
    let fork2_id = fork2["taskId"].as_str().unwrap().to_owned();
    let archived = workspace_call!(
        28,
        "workspace-archive",
        "manage_workspace",
        serde_json::json!({"action":"archiveThreads","taskIds":[fork_id,fork2_id],
        "idempotencyKey":"runtime-archive","closeTabs":true,"expectedLayoutRevision":fork2["placement"]["snapshot"]["revision"]})
    );
    assert_eq!(archived["allArchived"], true, "{archived}");
    assert_eq!(archived["outcomes"].as_array().unwrap().len(), 2);
    for id in [&fork_id, &fork2_id] {
        assert!(core.workspace.chat(id).unwrap().unwrap().archived);
    }
    assert!(!core.workspace.chat(&source).unwrap().unwrap().archived);
    assert!(!core.workspace.chat(&sibling).unwrap().unwrap().archived);
    let forbidden = call(
        29,
        "manage_workspace",
        serde_json::json!({"action":"archiveThreads","taskIds":[sibling],"idempotencyKey":"runtime-unrelated"}),
        &a.thread,
        &a.turn,
        "workspace-unrelated",
    );
    assert!(tool_text(&mcp_post(&http, &a.url, &a.token, &forbidden).await.1).0);

    // Chat f82212c7 (2026-10-06): an agent archived its helpers with
    // closeTabs:true and no revision. Views addressed only by chat ID must
    // apply to the current layout, and the exact retry must replay.
    let fork3 = workspace_call!(
        40,
        "workspace-fork-no-revision",
        "manage_workspace",
        serde_json::json!({"action":"forkThread","taskIds":[source],"idempotencyKey":"runtime-fork-three"})
    );
    let fork3_id = fork3["taskId"].as_str().unwrap().to_owned();
    let archive_without_revision = serde_json::json!({"action":"archiveThreads","taskIds":[fork3_id],
        "closeTabs":true,"idempotencyKey":"runtime-archive-no-revision"});
    let archived3 = workspace_call!(
        41,
        "workspace-archive-no-revision",
        "manage_workspace",
        archive_without_revision.clone()
    );
    assert_eq!(archived3["allArchived"], true, "{archived3}");
    assert!(archived3["layout"]["error"].is_null(), "{archived3}");
    let after_archive: luna_proto::ChatWorkbenchSnapshot =
        serde_json::from_value(archived3["layout"]["snapshot"].clone()).unwrap();
    assert!(!after_archive.contains_chat(&fork3_id));
    assert!(core.workspace.chat(&fork3_id).unwrap().unwrap().archived);
    let replayed3 = workspace_call!(
        42,
        "workspace-archive-no-revision-replay",
        "manage_workspace",
        archive_without_revision
    );
    assert!(replayed3["layout"]["error"].is_null(), "{replayed3}");
    assert_eq!(
        replayed3["layout"]["snapshot"],
        archived3["layout"]["snapshot"]
    );
    // A destination still names panes, so it still needs a fresh revision.
    let split_without_revision = call(
        43,
        "manage_workspace",
        serde_json::json!({"action":"openTab","taskIds":[child],"idempotencyKey":"runtime-split-no-revision",
        "destination":{"kind":"split","nodeId":after_archive.root.id(),"edge":"left",
        "newPaneId":uuid::Uuid::new_v4(),"splitId":uuid::Uuid::new_v4()}}),
        &a.thread,
        &a.turn,
        "workspace-split-no-revision",
    );
    let (failed, text) = tool_text(
        &mcp_post(&http, &a.url, &a.token, &split_without_revision)
            .await
            .1,
    );
    assert!(
        failed && text.starts_with("expectedLayoutRevision is required with destination"),
        "{text}"
    );
    // Same chat: the schema offered opencode, which this device lacks.
    let unavailable = call(
        44,
        "list_child_models",
        serde_json::json!({"provider":"opencode","query":"astra"}),
        &a.thread,
        &a.turn,
        "models-unavailable-provider",
    );
    let (failed, text) = tool_text(&mcp_post(&http, &a.url, &a.token, &unavailable).await.1);
    assert!(failed, "{text}");
    assert_eq!(
        text,
        "The opencode provider is not installed or enabled on this device. Available providers: codex."
    );
    if let Ok(directory) = std::env::var("AGENT_WORKSPACE_EVIDENCE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(std::path::Path::new(&directory).join("runtime-workspace.json"),serde_json::to_vec_pretty(&serde_json::json!({
            "provider":"deterministic Codex peer through production MCP", "workspace":workspace,"split":split,"replay":replay,"closed":closed,"fork":fork,"fork2":fork2,"archived":archived,
            "fork3":fork3,"archivedWithoutRevision":archived3,"archiveReplay":replayed3
        })).unwrap()).unwrap();
    }

    // Codex abandons a parked call (notifications/cancelled): the executor is
    // dropped, the card retired, and nothing is created.
    let abandoned_args = serde_json::json!({"prompt": "Abandoned brief", "idempotencyKey": "mcp-abandoned", "permissionMode":"full"});
    let abandoned = {
        let http = http.clone();
        let body = call(
            30,
            "create_thread",
            abandoned_args,
            &a.thread,
            &a.turn,
            "c-abandon",
        );
        let (url, token) = (a.url.clone(), a.token.clone());
        tokio::spawn(async move { mcp_post(&http, &url, &token, &body).await })
    };
    let request_id = next_approval(&mut events).await;
    let (status, _) = mcp_post(
        &http,
        &a.url,
        &a.token,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":30}}),
    )
    .await;
    assert_eq!(status, 202);
    let (failed, text) = tool_text(&abandoned.await.unwrap().1);
    assert!(failed && text.contains("cancelled"), "{text}");
    wait_resolved(&mut events, &request_id).await;
    assert_eq!(
        core.sessions.respond_luna_local_ui_approval(
            &source,
            &request_id,
            ApprovalDecision::AllowOnce
        ),
        luna_engine::sessions::LunaApprovalOutcome::Stale
    );
    assert!(
        !core.workspace.read_chats().unwrap().iter().any(|chat| chat
            .child_thread
            .as_ref()
            .is_some_and(|child| child.initial_prompt == "Abandoned brief")),
        "abandoned create_thread created a child"
    );

    // Cross-chat and cross-account: each Codex process (and so each account
    // home) has its own endpoint and token.
    let (_, mut other_events) = core.sessions.subscribe(&other, 0).unwrap();
    hold(other.clone()).await;
    let b = held_endpoint(&hold_dir, &other, &mut other_events).await;
    assert_ne!(a.url, b.url);
    assert_eq!(mcp_post(&http, &b.url, &a.token, &list).await.0, 401);
    assert_eq!(mcp_post(&http, &a.url, &b.token, &list).await.0, 401);

    // Depth limit: a fifth level is refused before any approval card.
    let (_, mut deep_events) = core.sessions.subscribe(&deep4, 0).unwrap();
    hold(deep4.clone()).await;
    let d = held_endpoint(&hold_dir, &deep4, &mut deep_events).await;
    let (failed, text) = tool_text(
        &mcp_post(
            &http,
            &d.url,
            &d.token,
            &call(
                40,
                "create_thread",
                serde_json::json!({"prompt":"too deep","idempotencyKey":"too-deep"}),
                &d.thread,
                &d.turn,
                "c-deep",
            ),
        )
        .await
        .1,
    );
    assert!(failed && text.contains("four levels"), "{text}");

    // Stop while create_thread is parked: the provider gets an error result
    // before the session retires its endpoint, and no child is created.
    let stopped = {
        let http = http.clone();
        let body = call(
            50,
            "create_thread",
            serde_json::json!({"prompt": "Stopped brief", "idempotencyKey": "mcp-stopped", "permissionMode":"full"}),
            &a.thread,
            &a.turn,
            "c-stop",
        );
        let (url, token) = (a.url.clone(), a.token.clone());
        tokio::spawn(async move { mcp_post(&http, &url, &token, &body).await })
    };
    next_approval(&mut events).await;
    assert!(core.sessions.interrupt_for_stop(&source).await.unwrap());
    let (failed, text) = tool_text(
        &tokio::time::timeout(std::time::Duration::from_secs(10), stopped)
            .await
            .expect("Stop answers the parked MCP call")
            .unwrap()
            .1,
    );
    assert!(failed, "{text}");
    assert!(
        !core.workspace.read_chats().unwrap().iter().any(|chat| chat
            .child_thread
            .as_ref()
            .is_some_and(|child| child.initial_prompt == "Stopped brief")),
        "Stop created a child"
    );

    // Retired binding: once the run ends, its endpoint and token are gone.
    for task in [&source, &other, &deep4] {
        std::fs::write(hold_dir.join(format!("release-{task}")), b"").unwrap();
    }
    wait_done(&mut events).await;
    let retired = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match http
                .post(&a.url)
                .bearer_auth(&a.token)
                .json(&list)
                .send()
                .await
            {
                Err(_) => break "refused",
                Ok(response) if response.status().as_u16() == 401 => break "unauthorized",
                Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("retired endpoint stops serving");
    assert!(matches!(retired, "refused" | "unauthorized"));
    core.shutdown().await;
}

async fn next_approval(
    events: &mut tokio::sync::broadcast::Receiver<luna_engine::JournaledEvent>,
) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let luna_proto::AgentEvent::InputRequested { request_id, .. } =
                events.recv().await.unwrap().event
            {
                return request_id;
            }
        }
    })
    .await
    .expect("LUNA approval requested")
}

async fn wait_resolved(
    events: &mut tokio::sync::broadcast::Receiver<luna_engine::JournaledEvent>,
    expected: &str,
) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let luna_proto::AgentEvent::InputResolved { request_id } =
                events.recv().await.unwrap().event
                && request_id == expected
            {
                return;
            }
        }
    })
    .await
    .expect("abandoned approval card retired");
}

async fn wait_done(events: &mut tokio::sync::broadcast::Receiver<luna_engine::JournaledEvent>) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let luna_proto::AgentEvent::Done { .. } = events.recv().await.unwrap().event {
                return;
            }
        }
    })
    .await
    .expect("held source turn settled");
}
