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

#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
                mcp: None,
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
                    details: None,
                }),
                input: None,
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

// Slice 6: specified before narrowing Activity's document read. The same
// full RPC sequence runs before/after; no timing limit in ordinary tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit isolated Activity performance and salvage E2E"]
#[cfg(unix)]
async fn long_history_attention_preserves_latest_assistant_and_measures_cost() {
    fn cpu_ms() -> f64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
    }
    let output = std::path::PathBuf::from(std::env::var("LUNA_PERF_EVIDENCE").unwrap());
    assert!(output.is_absolute());
    std::fs::create_dir_all(&output).unwrap();
    let mut workloads = Vec::new();
    for count in [1200, 4200] {
        let dir = tempfile::tempdir().unwrap();
        let core = assemble(dir.path());
        let device = core.device_id.clone();
        for chat in 0..6 {
            let id = format!("activity-perf-{chat}");
            core.workspace
                .create_chat(&id, None, Some(&device), Some(config()), None)
                .unwrap();
            let doc = core.doc_host.open(&id).unwrap();
            let mut entries = (0..count)
                .map(|i| SessionMessageEntry {
                    id: format!("history-{i}"),
                    role: MessageRole::Assistant,
                    parts: vec![MessagePart::Text {
                        id: "text".into(),
                        text: "settled history text ".repeat(40),
                    }],
                    created_at: i as i64,
                    device_id: device.clone(),
                    status: Some(MessageStatus::Complete),
                    continuation_of: None,
                })
                .collect::<Vec<_>>();
            // An old unresolved request is superseded by the later assistant.
            entries[0] = input("old-turn", "old-request", true, &device);
            if chat == 0 {
                entries.push(input("current-turn", "current-request", true, &device));
            }
            for role in [MessageRole::User, MessageRole::System] {
                entries.push(SessionMessageEntry {
                    id: format!("trailer-{role:?}"),
                    role,
                    parts: vec![MessagePart::Text {
                        id: "text".into(),
                        text: "trailing non-assistant".into(),
                    }],
                    created_at: 10000,
                    device_id: device.clone(),
                    status: Some(MessageStatus::Complete),
                    continuation_of: None,
                });
            }
            doc.doc()
                .replace_conversation_entries(&entries, &Default::default())
                .unwrap();
            doc.doc()
                .doc()
                .get_list("messages")
                .push("unsalvageable trailing slot")
                .unwrap();
            doc.doc().doc().commit();
        }
        let client = luna_rpc::memory_client(core.rpc_service());
        let first = snapshot(&client, &device).await;
        assert_eq!(
            only_needs(&first).request_id.as_deref(),
            Some("current-request")
        );
        assert!(claim(&client, &device, false).await.transitions.is_empty());
        // Let the independent initial snapshot debounce settle before timing.
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let cpu = cpu_ms();
        let start = std::time::Instant::now();
        for _ in 0..8 {
            let current = snapshot(&client, &device).await;
            assert_eq!(
                only_needs(&current).request_id.as_deref(),
                Some("current-request")
            );
            assert_eq!(current.revision, first.revision);
            assert!(claim(&client, &device, true).await.transitions.is_empty());
            assert!(!serde_json::to_string(&current).unwrap().contains("Private"));
        }
        let measurement = serde_json::json!({"historyEntriesPerChat":count,"chats":6,"pollPairs":8,"cpuMs":cpu_ms()-cpu,"wallMs":start.elapsed().as_secs_f64()*1000.0});
        let doc = core.doc_host.open("activity-perf-0").unwrap();
        let mut resolved = input("current-turn", "current-request", true, &device);
        if let MessagePart::Input { resolved, .. } = &mut resolved.parts[0] {
            *resolved = true;
        }
        doc.doc()
            .replace_conversation_entries(&[resolved], &Default::default())
            .unwrap();
        assert_eq!(snapshot(&client, &device).await.unresolved_count, 0);
        doc.doc()
            .push_message(&input("salvaged-turn", "salvaged-request", false, &device))
            .unwrap();
        let list = doc.doc().doc().get_list("messages");
        let Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) = list.get(1) else {
            panic!("assistant map")
        };
        map.delete("role").unwrap();
        map.delete("createdAt").unwrap();
        doc.doc().doc().commit();
        list.push(99).unwrap();
        doc.doc().doc().commit();
        assert_eq!(
            only_needs(&snapshot(&client, &device).await)
                .request_id
                .as_deref(),
            Some("salvaged-request")
        );
        let mut user = input("only-user", "ignored-request", true, &device);
        user.role = MessageRole::User;
        doc.doc()
            .replace_conversation_entries(&[user], &Default::default())
            .unwrap();
        assert_eq!(snapshot(&client, &device).await.unresolved_count, 0);
        doc.doc()
            .replace_conversation_entries(&[], &Default::default())
            .unwrap();
        assert_eq!(snapshot(&client, &device).await.unresolved_count, 0);
        workloads.push(measurement);
        drop(doc);
        drop(client);
        assert!(core.shutdown().await);
    }
    let evidence = serde_json::json!({"passed":true,"workloads":workloads,"latestApprovalOnly":true,"trailersAndMalformedSlots":true,"salvagedMissingRole":true,"resolutionThenNewRequest":true,"userOnlyAndEmpty":true,"noDuplicateOrPrivateNotification":true});
    std::fs::write(
        output.join("attention.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
}
