//! L15 behavior through real task docs, engine RPC, and the durable store.
//! No provider, Keychain, live profile, or native notification is contacted.

use std::sync::Arc;

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, QueueDeliveryGate, QueuedMessage, SessionMessageEntry,
};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{
    AttentionItem, AttentionKind, AttentionLifecycle, AttentionMutation, AttentionMutationRequest,
    AttentionMutationResult, AttentionSnapshot, AttentionTarget, AttentionTransitionBatch,
    ChatConfig, HarnessId, PermissionMode, SandboxLevel, UserInputQuestion,
};
use luna_rpc::{RpcClient, methods};

const TASK: &str = "l15-synthetic-task";

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("synthetic-attention-model".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Auto,
        effective_permission_mode: PermissionMode::Auto,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn assemble(path: &std::path::Path) -> EngineCore {
    EngineCore::assemble(
        path,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap()
}

fn input(id: &str, request: &str, approval: bool, device_id: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Input {
            id: request.into(),
            request_id: request.into(),
            questions: vec![UserInputQuestion {
                id: "question".into(),
                header: "Private header must stay in the transcript".into(),
                question: "Private question and secret must stay in the transcript".into(),
                options: vec!["private option".into()],
                multi_select: false,
                approval: approval.then(|| luna_proto::ApprovalRequest {
                    provider_request_id: request.into(),
                    provider: HarnessId::Mock,
                    turn_id: Some(id.into()),
                    machine_name: "synthetic".into(),
                    action_kind: luna_proto::ApprovalActionKind::Tool,
                    summary: "Private approval details".into(),
                    requested_scope: luna_proto::PermissionScope::Workspace,
                    expires_at_ms: 1_900_000_000_000,
                    supports_task_allowance: false,
                }),
            }],
            resolved: false,
        }],
        created_at: 1_780_000_000_000,
        device_id: device_id.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

async fn snapshot(client: &RpcClient, device: &str) -> AttentionSnapshot {
    client
        .call_as(
            methods::GET_ATTENTION,
            serde_json::json!({"targetDeviceId": device}),
        )
        .await
        .unwrap()
}

async fn claim(client: &RpcClient, device: &str, deliver: bool) -> AttentionTransitionBatch {
    client
        .call_as(
            methods::CLAIM_ATTENTION_TRANSITIONS,
            serde_json::json!({
                "targetDeviceId": device,
                "consumerId": "synthetic-ui",
                "deliverExisting": deliver,
            }),
        )
        .await
        .unwrap()
}

async fn mutate(
    client: &RpcClient,
    device: &str,
    command: &str,
    revision: u64,
    mutation: AttentionMutation,
) -> AttentionMutationResult {
    client
        .call_as(
            methods::MUTATE_ATTENTION,
            serde_json::to_value(AttentionMutationRequest {
                command_id: command.into(),
                expected_revision: revision,
                mutation,
                target_device_id: Some(device.into()),
            })
            .unwrap(),
        )
        .await
        .unwrap()
}

fn only_needs(snapshot: &AttentionSnapshot) -> &AttentionItem {
    assert_eq!(snapshot.unresolved_count, 1, "snapshot={snapshot:?}");
    assert_eq!(snapshot.needs_attention.len(), 1);
    &snapshot.needs_attention[0]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_lifecycle_dedup_persistence_and_exact_routing() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let device = core.device_id.clone();
    core.workspace
        .create_chat(TASK, None, Some(&device), Some(config()), None)
        .unwrap();
    let doc = core.doc_host.open(TASK).unwrap();
    doc.doc()
        .push_message(&input("turn-1", "request-1", false, &device))
        .unwrap();
    assert_eq!(doc.doc().read_entries().unwrap().len(), 1);
    let client = luna_rpc::memory_client(core.rpc_service());

    let first = snapshot(&client, &device).await;
    let initial = only_needs(&first);
    assert_eq!(initial.kind, AttentionKind::UserInputRequired);
    assert_eq!(initial.request_id.as_deref(), Some("request-1"));
    assert_eq!(
        initial.target,
        AttentionTarget::TaskRequest {
            task_id: TASK.into(),
            request_id: "request-1".into(),
            message_id: "turn-1".into(),
        }
    );
    let serialized = serde_json::to_string(&first).unwrap();
    for private in [
        "Private header",
        "Private question",
        "private option",
        "secret",
    ] {
        assert!(!serialized.contains(private));
    }
    assert_eq!(snapshot(&client, &device).await.revision, first.revision);
    assert!(claim(&client, &device, false).await.transitions.is_empty());
    assert!(claim(&client, &device, true).await.transitions.is_empty());

    let seen = mutate(
        &client,
        &device,
        "seen-1",
        first.revision,
        AttentionMutation::MarkSeen {
            item_ids: vec![initial.id.clone()],
        },
    )
    .await;
    assert_eq!(
        only_needs(&seen.snapshot).lifecycle,
        AttentionLifecycle::Unresolved
    );
    let acknowledged = mutate(
        &client,
        &device,
        "ack-1",
        seen.revision,
        AttentionMutation::Acknowledge {
            item_id: initial.id.clone(),
        },
    )
    .await;
    assert_eq!(
        only_needs(&acknowledged.snapshot).lifecycle,
        AttentionLifecycle::Acknowledged
    );
    let duplicate = mutate(
        &client,
        &device,
        "ack-1",
        first.revision,
        AttentionMutation::Acknowledge {
            item_id: initial.id.clone(),
        },
    )
    .await;
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.revision, acknowledged.revision);
    assert_eq!(snapshot(&client, &device).await.unresolved_count, 1);

    core.doc_host.flush_all();
    core.workspace.flush();
    drop(client);
    drop(doc);
    drop(core);
    let reopened = assemble(dir.path());
    let reopened_entries = reopened
        .doc_host
        .open(TASK)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(
        reopened_entries.len(),
        1,
        "reopened entries={reopened_entries:?}"
    );
    assert!(
        matches!(
            &reopened_entries[0].parts[0],
            MessagePart::Input {
                resolved: false,
                ..
            }
        ),
        "reopened entries={reopened_entries:?}"
    );
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let persisted = snapshot(&client, &device).await;
    assert_eq!(
        only_needs(&persisted).lifecycle,
        AttentionLifecycle::Acknowledged
    );
    assert!(claim(&client, &device, false).await.transitions.is_empty());
    assert!(claim(&client, &device, true).await.transitions.is_empty());
    let doc = reopened.doc_host.open(TASK).unwrap();
    assert!(doc.doc().resolve_input("request-1").unwrap());
    let resolved = snapshot(&client, &device).await;
    assert_eq!(resolved.unresolved_count, 0);
    assert_eq!(resolved.recent.len(), 1);
    assert_eq!(resolved.recent[0].lifecycle, AttentionLifecycle::Resolved);
    assert!(resolved.recent[0].acknowledged_at_ms.is_some());
    assert_eq!(claim(&client, &device, true).await.transitions.len(), 1);
    assert!(claim(&client, &device, true).await.transitions.is_empty());
    let resolved_revision = resolved.revision;
    assert_eq!(snapshot(&client, &device).await.revision, resolved_revision);

    doc.doc()
        .push_message(&input("turn-2", "request-2", false, &device))
        .unwrap();
    let later = snapshot(&client, &device).await;
    assert_ne!(only_needs(&later).id, initial.id);
    assert_eq!(later.recent.len(), 1);
    assert_eq!(claim(&client, &device, true).await.transitions.len(), 1);
    assert!(
        client
            .call(
                methods::GET_ATTENTION,
                serde_json::json!({"targetDeviceId": "other-device"}),
            )
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_queue_completion_and_concurrent_claims_use_real_rpc_sources() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let device = core.device_id.clone();
    for id in ["approval-task", "queue-task", "completed-task"] {
        core.workspace
            .create_chat(id, None, Some(&device), Some(config()), None)
            .unwrap();
    }
    core.doc_host
        .open("approval-task")
        .unwrap()
        .doc()
        .push_message(&input("approval-turn", "approval-request", true, &device))
        .unwrap();
    let mut queue = QueuedMessage::new(
        "queue-row",
        "Private queued payload must not become a notification",
        &device,
    );
    queue.delivery_gate = Some(QueueDeliveryGate::ReviewRequired {
        previous_lease_id: "synthetic-lease".into(),
        owner_device_id: device.clone(),
        since_ms: 10,
        base_text_hash: "private-hash".into(),
    });
    core.doc_host
        .open("queue-task")
        .unwrap()
        .doc()
        .push_queued(&queue)
        .unwrap();
    core.workspace.record_session(&luna_proto::Session {
        chat_id: "completed-task".into(),
        device_id: device.clone(),
        status: luna_proto::SessionStatus::Idle,
        started_at: None,
        updated_at: chrono::Utc::now(),
        last_completed_turn: Some("completed-turn".into()),
        recovery: None,
    });
    let client = luna_rpc::memory_client(core.rpc_service());
    let initial = snapshot(&client, &device).await;
    assert_eq!(initial.unresolved_count, 2);
    assert_eq!(initial.recent.len(), 1);
    assert!(
        initial
            .needs_attention
            .iter()
            .any(|item| item.kind == AttentionKind::ApprovalRequired)
    );
    assert!(
        initial
            .needs_attention
            .iter()
            .any(|item| item.kind == AttentionKind::QueuedSendFailed)
    );
    assert_eq!(initial.recent[0].kind, AttentionKind::TaskCompleted);
    let serialized = serde_json::to_string(&initial).unwrap();
    for private in ["Private", "private option", "private-hash"] {
        assert!(!serialized.contains(private));
    }

    let (a, b) = tokio::join!(claim(&client, &device, true), claim(&client, &device, true));
    assert_eq!(a.transitions.len() + b.transitions.len(), 3);
    assert!(claim(&client, &device, true).await.transitions.is_empty());
    core.doc_host
        .open("approval-task")
        .unwrap()
        .doc()
        .resolve_input("approval-request")
        .unwrap();
    core.doc_host
        .open("queue-task")
        .unwrap()
        .doc()
        .remove_queued("queue-row")
        .unwrap();
    let settled = snapshot(&client, &device).await;
    assert_eq!(settled.unresolved_count, 0);
    assert_eq!(settled.recent.len(), 3);
    let cleared = mutate(
        &client,
        &device,
        "clear-recent",
        settled.revision,
        AttentionMutation::ClearRecent,
    )
    .await;
    assert_eq!(cleared.snapshot.unresolved_count, 0);
    assert!(cleared.snapshot.recent.is_empty());
    assert!(snapshot(&client, &device).await.recent.is_empty());
    core.doc_host.flush_all();
    core.workspace.flush();
    drop(client);
    drop(core);
    let reopened = assemble(dir.path());
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let persisted = snapshot(&client, &device).await;
    assert_eq!(persisted.unresolved_count, 0);
    assert!(persisted.recent.is_empty());
    assert!(claim(&client, &device, false).await.transitions.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_session_survives_restart_and_resolves_only_on_settled_status() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let device = core.device_id.clone();
    core.workspace
        .create_chat("recovery-task", None, Some(&device), Some(config()), None)
        .unwrap();
    let now = chrono::Utc::now();
    let mut session = luna_proto::Session {
        chat_id: "recovery-task".into(),
        device_id: device.clone(),
        status: luna_proto::SessionStatus::Errored,
        started_at: None,
        updated_at: now,
        last_completed_turn: None,
        recovery: None,
    };
    core.workspace.record_session(&session);
    let client = luna_rpc::memory_client(core.rpc_service());
    assert_eq!(
        only_needs(&snapshot(&client, &device).await).kind,
        AttentionKind::TaskFailed
    );
    core.workspace.flush();
    drop(client);
    drop(core);

    let reopened = assemble(dir.path());
    let client = luna_rpc::memory_client(reopened.rpc_service());
    assert_eq!(
        only_needs(&snapshot(&client, &device).await).kind,
        AttentionKind::TaskFailed
    );
    session.status = luna_proto::SessionStatus::Idle;
    session.updated_at = now + chrono::Duration::milliseconds(1);
    reopened.workspace.record_session(&session);
    let settled = snapshot(&client, &device).await;
    assert_eq!(settled.unresolved_count, 0);
    assert_eq!(settled.recent.len(), 1);
    assert_eq!(settled.recent[0].kind, AttentionKind::TaskFailed);
    session.status = luna_proto::SessionStatus::Errored;
    session.updated_at = now + chrono::Duration::milliseconds(2);
    reopened.workspace.record_session(&session);
    let later_failure = snapshot(&client, &device).await;
    assert_ne!(only_needs(&later_failure).id, settled.recent[0].id);
}
