//! L25 local backend evidence using only explicit synthetic homes and rows.

use std::collections::HashSet;
use std::sync::Arc;

use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use zeron_engine::cross_task_tools::{
    CrossTaskAuthority, CrossTaskControlGrant, CrossTaskError, CrossTaskTools,
};
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use zeron_proto::{
    ACTIVITY_ENVELOPE_VERSION, ActivityEnvelope, ActivityKind, ActivityOutcome, ActivityRedaction,
    ActivitySourceRef, CrossTaskControlAction, CrossTaskControlRequest, CrossTaskListRequest,
    CrossTaskReadRequest, CrossTaskSendMode, CrossTaskSendRequest, CrossTaskWaitRequest,
    CrossTaskWaitTarget, HarnessId, RuntimeDelivery, Session, SessionStatus, TaskLineage,
};

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
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
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
        delivery: RuntimeDelivery::CodexDynamicTools,
    };
    let tools = CrossTaskTools::new(rpc);
    let (attached, omitted) = tools.catalog(&authority);
    let attached_names = attached
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(
        attached_names,
        HashSet::from(["list_threads", "read_thread", "wait_threads"])
    );
    assert!(
        attached
            .iter()
            .all(|tool| tool.required_scopes == ["threads.read"])
    );
    assert!(
        omitted
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
        zeron_proto::CommandReceiptStatus::Applied
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
        zeron_proto::CommandReceiptStatus::Applied
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
        zeron_proto::CommandReceiptStatus::Applied
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
        zeron_proto::CommandReceiptStatus::Applied
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
        zeron_proto::CommandReceiptStatus::Conflict
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
        zeron_proto::CommandReceiptStatus::Applied
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
            status: zeron_proto::CommandReceiptStatus::Conflict,
            ..
        }
    ));
}
