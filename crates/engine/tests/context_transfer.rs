//! Portable context at the real RPC -> command -> document/store -> provider boundary.
//! Failure cases precede implementation in docs/design/harness-evolution/slice-2.md.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest};
use luna_rpc::RpcClient;
use serde_json::{Value, json};

const PARENT: &str = "11111111-1111-4111-8111-111111111111";
const FORK: &str = "22222222-2222-4222-8222-222222222222";

struct TransferPeer {
    provider: HarnessId,
    seen: Arc<Mutex<Vec<RunRequest>>>,
    uncertain: Arc<AtomicBool>,
    linger: bool,
}

#[async_trait]
impl Harness for TransferPeer {
    fn id(&self) -> HarnessId {
        self.provider
    }
    fn display_name(&self) -> &str {
        "Context transfer protocol peer"
    }
    fn supports_steering(&self) -> bool {
        self.linger
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        luna_proto::SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let mut seen = self.seen.lock().unwrap();
        seen.push(request.clone());
        if self.uncertain.load(Ordering::SeqCst) {
            return Err(HarnessError::Protocol("acceptance response lost".into()));
        }
        let id = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("native-context-{}", seen.len()));
        let events = futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: self.provider,
                model: "fixture".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: id.clone(),
                assistant_message_id: format!("reply-{}", seen.len()),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("Observed: {}", request.prompt),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(id.clone()),
            }),
        ]);
        if self.linger {
            return Ok(events
                .chain(futures::stream::once(async move {
                    controls.interrupt.cancelled().await;
                    drop(controls);
                    Ok(AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: Some(id),
                    })
                }))
                .boxed());
        }
        Ok(events.boxed())
    }
}

fn core(
    root: &std::path::Path,
    seen: Arc<Mutex<Vec<RunRequest>>>,
    uncertain: Arc<AtomicBool>,
) -> EngineCore {
    core_with_linger(root, seen, uncertain, false)
}

fn core_with_linger(
    root: &std::path::Path,
    seen: Arc<Mutex<Vec<RunRequest>>>,
    uncertain: Arc<AtomicBool>,
    linger: bool,
) -> EngineCore {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("luna_engine=warn")
        .with_test_writer()
        .try_init();
    let registry = HarnessRegistry::new();
    for provider in [HarnessId::Mock, HarnessId::ClaudeCode] {
        registry.register(Arc::new(TransferPeer {
            provider,
            seen: seen.clone(),
            uncertain: uncertain.clone(),
            linger: linger && provider == HarnessId::Mock,
        }));
    }
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    std::fs::write(&accounts.claude_config_file, r#"{"oauthAccount":{"accountUuid":"context-fixture-account","emailAddress":"context@example.invalid"}}"#).unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "context-transfer", "context-transfer"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap()
}

async fn call(client: &RpcClient, method: &str, params: Value) -> Value {
    client.call_as(method, params).await.unwrap()
}

async fn command(client: &RpcClient, task: &str, id: &str, intent: Value) -> Value {
    let basis = call(client, "GetTaskCommandSnapshot", json!({"taskId":task})).await;
    call(client, "ExecuteTaskCommand", json!({"taskId":task,"commandId":id,"actorId":"qa-user","clientId":"qa-context","clientSessionId":"qa-context","expectedTaskRevision":basis["snapshot"]["taskRevision"],"expectedBindingGeneration":basis["snapshot"]["bindingGeneration"],"intent":intent})).await
}

async fn create(client: &RpcClient, owner: &str, task: &str, source: Option<&str>) {
    call(
        client,
        "Mutate",
        json!({"op":"createSpace","spaceId":"context-project","deviceId":owner,"path":"/tmp"}),
    )
    .await;
    let receipt = command(client, task, &format!("create-{task}"), json!({"kind":"createChat","spaceId":"context-project","cwd":"/tmp","config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"},"lineage":{"sourceTaskId":source}})).await;
    assert_eq!(receipt["status"], "applied");
}

fn text(core: &EngineCore, task: &str, id: &str, content: &str, hidden: bool) {
    let mut parts = vec![MessagePart::Text {
        id: "text".into(),
        text: content.into(),
    }];
    if hidden {
        parts.push(MessagePart::Reasoning {
            id: "thought".into(),
            text: "PRIVATE_REASONING_SENTINEL".into(),
        });
    }
    core.doc_host
        .open(task)
        .unwrap()
        .doc()
        .push_message(&SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts,
            created_at: 100,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
}

async fn run(client: &RpcClient, engine: &EngineCore, task: &str, message: &str) {
    call(client, "QueueMessage", json!({"chatId":task,"messageId":message,"text":"Continue with the shared findings. Reply briefly.","holdForTurnEnd":true})).await;
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let rows = engine
                .doc_host
                .open(task)
                .unwrap()
                .doc()
                .read_entries()
                .unwrap();
            if let Some(index) = rows.iter().position(|entry| entry.id == message)
                && !engine.sessions.turn_in_flight(task)
                && rows[index + 1..].iter().any(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Complete)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        evidence(
            &format!("timeout-{message}"),
            &json!({"context":engine.doc_host.context_control(task).unwrap(),"messages":engine.doc_host.open(task).unwrap().doc().read_entries().unwrap(),"queue":engine.doc_host.open(task).unwrap().doc().read_queue().unwrap(),"commands":call(client,"GetTaskCommandSnapshot",json!({"taskId":task})).await}),
        );
        panic!("destination turn did not complete: {message}");
    }
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CONTEXT_TRANSFER_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_transfer_preview_commit_delivery_restart_and_delta() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let uncertain = Arc::new(AtomicBool::new(false));
    let engine = core(root.path(), seen.clone(), uncertain.clone());
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    create(&client, &engine.device_id, FORK, Some(PARENT)).await;
    text(&engine, FORK, "finding-1", "FORK_ONLY_MARKER_7021", true);
    // A persisted limit without a matching live runtime is historical; it
    // cannot establish the selected model's current capacity.
    engine
        .doc_host
        .open(PARENT)
        .unwrap()
        .doc()
        .update_context_usage(Some(10), Some(1_000_000))
        .unwrap();
    let request = json!({"kind":"forkMerge","sourceTaskId":FORK,"targetTaskId":PARENT,"promptText":"Continue with the shared findings. Reply briefly."});
    let before = call(&client, "GetTaskCommandSnapshot", json!({"taskId":PARENT})).await;
    let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
    evidence("preview", &preview);
    assert_eq!(
        before,
        call(&client, "GetTaskCommandSnapshot", json!({"taskId":PARENT})).await
    );
    assert_eq!(seen.lock().unwrap().len(), 0);
    assert!(
        preview["text"]
            .as_str()
            .unwrap()
            .contains("FORK_ONLY_MARKER_7021")
    );
    assert!(!preview.to_string().contains("PRIVATE_REASONING_SENTINEL"));
    assert_eq!(preview["included"].as_array().unwrap().len(), 1);
    assert_eq!(preview["budget"]["capacitySource"], "conservativeUnknown");
    let intent =
        json!({"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]});
    let applied = command(&client, PARENT, "merge-once", intent.clone()).await;
    assert_eq!(applied["status"], "applied", "{applied}");
    let duplicate = command(&client, PARENT, "merge-again", intent).await;
    assert_eq!(duplicate["status"], "applied", "{duplicate}");
    assert!(
        client
            .call_as::<Value>("PreviewContextTransfer", request.clone())
            .await
            .is_err(),
        "the same source must not get a second pending copy before its first transfer is delivered"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        0,
        "apply cannot start a parent turn"
    );
    let pending = call(&client, "ListContextTransfers", json!({"taskId":PARENT})).await;
    assert_eq!(pending["items"].as_array().unwrap().len(), 1);
    assert_eq!(pending["items"][0]["status"], "planned");
    run(&client, &engine, PARENT, "parent-first").await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    let prompt = seen.lock().unwrap()[0].prompt.clone();
    assert_eq!(prompt.matches("FORK_ONLY_MARKER_7021").count(), 1);
    assert!(!prompt.contains("PRIVATE_REASONING_SENTINEL"));
    let user = engine
        .doc_host
        .open(PARENT)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == "parent-first")
        .unwrap();
    let visible_prompt = user
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        visible_prompt, "Continue with the shared findings. Reply briefly.",
        "captured context must not replace the user's visible prompt or multiply shared transcript payloads"
    );
    let delivered = call(&client, "ListContextTransfers", json!({"taskId":PARENT})).await;
    assert!(
        matches!(
            delivered["items"][0]["status"].as_str(),
            Some("delivered" | "consumed")
        ),
        "{delivered}"
    );
    evidence("delivered", &delivered);
    assert_eq!(delivered["items"][0]["status"], "consumed");
    engine.shutdown().await;
    drop(client);
    drop(engine);
    let reopened = core(root.path(), seen.clone(), uncertain);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    text(&reopened, FORK, "finding-2", "SECOND_DELTA_MARKER", false);
    let delta = call(&client, "PreviewContextTransfer", request.clone()).await;
    assert!(
        delta["text"]
            .as_str()
            .unwrap()
            .contains("SECOND_DELTA_MARKER")
    );
    assert!(
        !delta["text"]
            .as_str()
            .unwrap()
            .contains("FORK_ONLY_MARKER_7021")
    );
    assert_eq!(delta["alreadyDelivered"], json!(["finding-1"]));
    run(&client, &reopened, PARENT, "parent-after-restart").await;
    assert!(
        !seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .prompt
            .contains("FORK_ONLY_MARKER_7021")
    );
    evidence("delta-after-restart", &delta);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_transfer_budget_stale_preview_and_deleted_source() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = core(root.path(), seen.clone(), Arc::new(AtomicBool::new(false)));
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    create(&client, &engine.device_id, FORK, Some(PARENT)).await;
    for index in 0..18 {
        text(
            &engine,
            FORK,
            &format!("finding-{index}"),
            &format!("FINDING_{index} {}", "🦉".repeat(2400)),
            false,
        );
    }
    let request =
        json!({"kind":"forkMerge","sourceTaskId":FORK,"targetTaskId":PARENT,"promptText":"short"});
    let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
    let serialized = preview["text"].as_str().unwrap();
    assert!(serialized.len() <= preview["budget"]["maxBytes"].as_u64().unwrap() as usize);
    assert!(!preview["omitted"].as_array().unwrap().is_empty());
    let mut huge_request = request.clone();
    huge_request["promptText"] = "x".repeat(200_000).into();
    let huge = client
        .call_as::<Value>("PreviewContextTransfer", huge_request)
        .await;
    assert!(huge.is_err());
    text(
        &engine,
        PARENT,
        "parent-advanced",
        "Parent advanced after preview",
        false,
    );
    let stale = command(
        &client,
        PARENT,
        "stale-transfer",
        json!({"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]}),
    )
    .await;
    assert_eq!(stale["status"], "conflict", "{stale}");
    assert_eq!(seen.lock().unwrap().len(), 0);
    let fresh = call(&client, "PreviewContextTransfer", request.clone()).await;
    let applied = command(
        &client,
        PARENT,
        "fresh-transfer",
        json!({"kind":"applyContextTransfer","request":request,"previewId":fresh["previewId"]}),
    )
    .await;
    assert_eq!(applied["status"], "applied");
    let deleted = command(&client, FORK, "delete-source", json!({"kind":"delete"})).await;
    assert_eq!(deleted["status"], "applied");
    run(&client, &engine, PARENT, "after-delete").await;
    assert!(
        seen.lock()
            .unwrap()
            .last()
            .unwrap()
            .prompt
            .contains("FINDING_")
    );
    let omitted = call(
        &client,
        "ReadContextTransferItems",
        json!({"taskId":PARENT,"transferId":fresh["previewId"],"offset":0,"limit":10}),
    )
    .await;
    assert!(
        omitted["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["available"] == false)
    );
    evidence(
        "budget-and-source-deletion",
        &json!({"preview":fresh,"items":omitted,"stale":stale}),
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_transfer_unknown_acceptance_survives_restart_without_reinjection() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let uncertain = Arc::new(AtomicBool::new(true));
    let engine = core(root.path(), seen.clone(), uncertain.clone());
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    create(&client, &engine.device_id, FORK, Some(PARENT)).await;
    text(
        &engine,
        FORK,
        "uncertain-source",
        "UNCERTAIN_CONTEXT_ONCE",
        false,
    );
    let request = json!({"kind":"forkMerge","sourceTaskId":FORK,"targetTaskId":PARENT});
    let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
    assert_eq!(command(&client, PARENT, "unknown-plan", json!({"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]})).await["status"], "applied");
    call(&client, "QueueMessage", json!({"chatId":PARENT,"messageId":"uncertain-send","text":"Use the findings","holdForTurnEnd":true})).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        while seen.lock().unwrap().is_empty() || engine.sessions.turn_in_flight(PARENT) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let unknown = call(&client, "ListContextTransfers", json!({"taskId":PARENT})).await;
    assert_eq!(unknown["items"][0]["status"], "unknown");
    engine.shutdown().await;
    drop(client);
    drop(engine);
    uncertain.store(false, Ordering::SeqCst);
    let reopened = core(root.path(), seen.clone(), uncertain);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    assert_eq!(
        call(&client, "ListContextTransfers", json!({"taskId":PARENT})).await["items"][0]["status"],
        "unknown"
    );
    assert!(
        client
            .call_as::<Value>("PreviewContextTransfer", request)
            .await
            .is_err()
    );
    let attempted = command(&client, PARENT, "new-send-during-unknown", json!({"kind":"session","command":{"kind":"run","messageId":"second-unknown","request":{"prompt":"Do not replay the prior payload","harness":"mock","model":"fixture","cwd":"/tmp","sandbox":"workspace-write","permissionMode":"ask","autoApprove":false,"resume":null,"attachments":[],"worktree":null}}})).await;
    assert!(matches!(
        attempted["status"].as_str(),
        Some("pending" | "applied" | "failed" | "rejected")
    ));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let receipt = call(
                &client,
                "GetTaskCommandReceipt",
                json!({"commandId":"new-send-during-unknown"}),
            )
            .await;
            if matches!(
                receipt["status"].as_str(),
                Some("failed" | "rejected" | "conflict")
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "an unknown transfer cannot reach the provider twice"
    );
    evidence("unknown-after-restart", &unknown);
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn portable_context_and_provider_roundtrip_keep_identity_and_explicit_delivery() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = core(root.path(), seen.clone(), Arc::new(AtomicBool::new(false)));
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    create(&client, &engine.device_id, FORK, Some(PARENT)).await;
    text(
        &engine,
        PARENT,
        "initial-marker",
        "ROUNDTRIP_VISIBLE_884",
        true,
    );
    let portable_request = json!({"kind":"portableFork","sourceTaskId":PARENT,"targetTaskId":FORK});
    let portable = call(&client, "PreviewContextTransfer", portable_request.clone()).await;
    assert_eq!(command(&client, FORK, "portable-plan", json!({"kind":"applyContextTransfer","request":portable_request,"previewId":portable["previewId"]})).await["status"], "applied");
    assert!(seen.lock().unwrap().is_empty());
    run(&client, &engine, FORK, "portable-first").await;
    assert!(
        seen.lock().unwrap()[0]
            .prompt
            .contains("ROUNDTRIP_VISIBLE_884")
    );
    let first_binding = engine
        .task_identity
        .view(FORK)
        .unwrap()
        .current_binding
        .unwrap();
    let mut transitions = Vec::new();
    for (provider, command_id, message) in [
        ("claude-code", "to-b", "b-turn"),
        ("mock", "back-to-a", "a-return"),
    ] {
        let request = json!({"kind":"providerHandoff","sourceTaskId":FORK,"targetTaskId":FORK,"targetConfig":{"harness":provider,"model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}});
        let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
        let before_runs = seen.lock().unwrap().len();
        let result = command(&client, FORK, command_id, json!({"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]})).await;
        assert_eq!(result["status"], "applied", "{result}");
        assert_eq!(seen.lock().unwrap().len(), before_runs);
        run(&client, &engine, FORK, message).await;
        assert!(
            seen.lock()
                .unwrap()
                .last()
                .unwrap()
                .prompt
                .contains("ROUNDTRIP_VISIBLE_884")
        );
        assert!(
            !seen
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .prompt
                .contains("PRIVATE_REASONING_SENTINEL")
        );
        transitions.push(call(&client, "GetTaskIdentity", json!({"taskId":FORK})).await);
    }
    let returned = engine
        .task_identity
        .view(FORK)
        .unwrap()
        .current_binding
        .unwrap();
    assert!(returned.generation > first_binding.generation);
    assert_ne!(returned.provider_task_id, first_binding.provider_task_id);
    assert_eq!(
        engine
            .task_identity
            .view(FORK)
            .unwrap()
            .task
            .lineage
            .source_task_id
            .as_deref(),
        Some(PARENT)
    );
    evidence(
        "portable-and-provider-roundtrip",
        &json!({"portable":portable,"bindings":transitions}),
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_transfer_payload_and_projection_rollback_with_receipt_failure() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = core(root.path(), seen.clone(), Arc::new(AtomicBool::new(false)));
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    create(&client, &engine.device_id, FORK, Some(PARENT)).await;
    text(
        &engine,
        FORK,
        "atomic-source",
        "ATOMIC_VISIBLE_FINDING",
        false,
    );
    let request = json!({"kind":"forkMerge","sourceTaskId":FORK,"targetTaskId":PARENT});
    let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
    let db = rusqlite::Connection::open(
        root.path()
            .join("data/orgs/context-transfer/context-transfer/docs.sqlite3"),
    )
    .unwrap();
    db.execute_batch("CREATE TRIGGER fail_transfer BEFORE INSERT ON task_command_events WHEN NEW.command_id='atomic-transfer' AND json_extract(NEW.receipt_json, '$.status')='applied' BEGIN SELECT RAISE(ABORT, 'forced transfer commit failure'); END;").unwrap();
    let basis = call(&client, "GetTaskCommandSnapshot", json!({"taskId":PARENT})).await;
    let attempted = client.call_as::<Value>("ExecuteTaskCommand", json!({"taskId":PARENT,"commandId":"atomic-transfer","expectedTaskRevision":basis["snapshot"]["taskRevision"],"expectedBindingGeneration":basis["snapshot"]["bindingGeneration"],"intent":{"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]}})).await;
    assert!(attempted.is_err(), "{attempted:?}");
    assert!(
        call(&client, "ListContextTransfers", json!({"taskId":PARENT})).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .doc_host
            .context_control(PARENT)
            .unwrap()
            .context_transfers
            .is_empty()
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM snapshots WHERE doc_id LIKE 'luna-context-transfer:%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(seen.lock().unwrap().is_empty());
    evidence(
        "atomic-rollback",
        &json!({"error":attempted.unwrap_err().to_string(),"payloadRecords":0,"providerRequests":0}),
    );
    db.execute_batch("DROP TRIGGER fail_transfer").unwrap();
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn portable_new_task_preview_creates_nothing_and_apply_is_atomic() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = core(root.path(), seen.clone(), Arc::new(AtomicBool::new(false)));
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    text(
        &engine,
        PARENT,
        "portable-seed",
        "PORTABLE_NEW_TASK_MARKER",
        true,
    );
    let request = json!({"kind":"portableFork","sourceTaskId":PARENT,"targetTaskId":FORK,
        "newTarget":{"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"},"title":"Independent context task"}});
    let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
    assert_eq!(preview["target"]["taskRevision"], 0);
    assert!(engine.workspace.chat(FORK).unwrap().is_none());
    assert!(engine.task_identity.view(FORK).is_none());
    assert!(seen.lock().unwrap().is_empty());
    let intent =
        json!({"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]});
    let applied = command(&client, FORK, "create-portable-context", intent.clone()).await;
    assert_eq!(applied["status"], "applied", "{applied}");
    assert!(
        engine
            .workspace
            .chat(FORK)
            .unwrap()
            .unwrap()
            .child_thread
            .is_none()
    );
    let identity = engine.task_identity.view(FORK).unwrap();
    assert!(identity.current_binding.is_none());
    assert_eq!(
        identity.task.lineage.source_task_id.as_deref(),
        Some(PARENT)
    );
    assert_eq!(
        identity.task.lineage.lifecycle_owner_task_id.as_deref(),
        Some(FORK)
    );
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(
        command(&client, FORK, "recover-portable-context", intent).await["status"],
        "applied"
    );
    assert_eq!(
        call(&client, "ListContextTransfers", json!({"taskId":FORK})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    run(&client, &engine, FORK, "portable-created-first-send").await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(
        seen.lock().unwrap()[0]
            .prompt
            .contains("PORTABLE_NEW_TASK_MARKER")
    );
    evidence(
        "portable-new-task",
        &json!({"preview":preview,"receipt":applied,"identityBeforeSend":identity}),
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_message_delta_continues_after_delivered_utf8_range() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = core(root.path(), seen.clone(), Arc::new(AtomicBool::new(false)));
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    create(&client, &engine.device_id, FORK, Some(PARENT)).await;
    let original = format!("FIRST_PREFIX_1432 {} END_LAST_8765", "🤖".repeat(15000));
    text(&engine, FORK, "long-finding", &original, false);
    let request = json!({"kind":"forkMerge","sourceTaskId":FORK,"targetTaskId":PARENT});
    let first = call(&client, "PreviewContextTransfer", request.clone()).await;
    assert_eq!(first["included"][0]["truncated"], true);
    assert_eq!(
        command(
            &client,
            PARENT,
            "first-part",
            json!({"kind":"applyContextTransfer","request":request,"previewId":first["previewId"]})
        )
        .await["status"],
        "applied"
    );
    run(&client, &engine, PARENT, "first-part-send").await;
    let next = call(&client, "PreviewContextTransfer", request.clone()).await;
    assert!(
        !next["text"].as_str().unwrap().contains("FIRST_PREFIX_1432"),
        "the already-delivered prefix cannot be injected again"
    );
    assert_eq!(
        next["included"][0]["startByte"],
        first["included"][0]["endByte"]
    );
    assert!(next["included"][0]["startByte"].as_u64().unwrap() > 0);
    let mut cursor = first["included"][0]["endByte"].as_u64().unwrap();
    let start = cursor as usize;
    let mut retrieved = String::new();
    loop {
        let page = call(&client, "ReadContextTransferItems", json!({"taskId":PARENT,"transferId":first["previewId"],"messageId":"long-finding","textOffset":cursor})).await;
        let item = &page["items"][0];
        assert_eq!(item["available"], true);
        assert_eq!(item["startByte"], cursor);
        retrieved.push_str(item["text"].as_str().unwrap());
        let Some(next_cursor) = item["nextTextOffset"].as_u64() else {
            break;
        };
        assert!(next_cursor > cursor);
        cursor = next_cursor;
    }
    assert_eq!(retrieved, original[start..]);
    evidence("partial-delta", &json!({"first":first,"next":next}));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn warm_provider_retirement_cannot_restore_resume_across_fresh_binding() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let engine = core_with_linger(
        root.path(),
        seen.clone(),
        Arc::new(AtomicBool::new(false)),
        true,
    );
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine.device_id, PARENT, None).await;
    run(&client, &engine, PARENT, "old-provider-first").await;
    assert_eq!(
        call(&client, "GetRuntimeContract", json!({"taskId":PARENT})).await["execution"]["phase"],
        "warm"
    );
    let old = call(&client, "GetTaskIdentity", json!({"taskId":PARENT})).await;
    let request = json!({"kind":"providerHandoff","sourceTaskId":PARENT,"targetTaskId":PARENT,
        "targetConfig":{"harness":"claude-code","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}});
    let preview = call(&client, "PreviewContextTransfer", request.clone()).await;
    let applied = command(
        &client,
        PARENT,
        "warm-provider-switch",
        json!({"kind":"applyContextTransfer","request":request,"previewId":preview["previewId"]}),
    )
    .await;
    assert_eq!(applied["status"], "applied", "{applied}");
    run(&client, &engine, PARENT, "new-provider-first").await;
    let requests = seen.lock().unwrap().clone();
    evidence(
        "warm-provider-retirement",
        &json!({"oldIdentity":old,"requests":requests,
        "receipt":call(&client,"GetContextTransfer",json!({"taskId":PARENT,"transferId":preview["previewId"]})).await}),
    );
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].resume, None,
        "A fresh managed binding must override the old provider's late resume cache"
    );
    engine.shutdown().await;
}
