//! RPC -> durable queue/receipt -> provider boundary -> engine restart.
//! Failure cases were written first in docs/design/harness-evolution/slice-4.md.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest};
use luna_rpc::RpcClient;
use serde_json::{Value, json};

const CHAT: &str = "a4444444-4444-4444-8444-444444444444";

struct Peer {
    provider: HarnessId,
    seen: Arc<Mutex<Vec<String>>>,
    uncertain: Arc<AtomicBool>,
    interrupted: Arc<AtomicBool>,
}

#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        self.provider
    }
    fn display_name(&self) -> &str {
        "Preflight protocol peer"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        luna_proto::SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.seen.lock().unwrap().push(request.prompt.clone());
        if self.uncertain.load(Ordering::SeqCst) {
            return Err(HarnessError::Protocol(
                "acceptance response lost after provider entry".into(),
            ));
        }
        let started = AgentEvent::SessionStarted {
            harness: self.provider,
            model: "fixture".into(),
            tools: vec![],
            cwd: request.cwd,
            session_id: "preflight-native".into(),
            assistant_message_id: format!("reply-{}", request.prompt),
        };
        let interrupted = self.interrupted.clone();
        let hold = request.prompt == "hold active turn";
        Ok(futures::stream::once(async move { Ok(started) })
            .chain(futures::stream::once(async move {
                if hold {
                    controls.interrupt.cancelled().await;
                    interrupted.store(true, Ordering::SeqCst);
                }
                Ok(AgentEvent::Done {
                    status: if hold {
                        DoneStatus::Interrupted
                    } else {
                        DoneStatus::Completed
                    },
                    result: None,
                    error: None,
                    session_id: Some("preflight-native".into()),
                })
            }))
            .boxed())
    }
}

fn core(
    root: &std::path::Path,
    seen: Arc<Mutex<Vec<String>>>,
    uncertain: Arc<AtomicBool>,
    interrupted: Arc<AtomicBool>,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    for provider in [HarnessId::Mock, HarnessId::ClaudeCode] {
        registry.register(Arc::new(Peer {
            provider,
            seen: seen.clone(),
            uncertain: uncertain.clone(),
            interrupted: interrupted.clone(),
        }));
    }
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "queue-preflight", "queue-preflight"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap()
}

fn repair_account(root: &std::path::Path) {
    std::fs::write(root.join("claude.json"), r#"{"oauthAccount":{"accountUuid":"synthetic-preflight-account","emailAddress":"preflight@example.invalid"}}"#).unwrap();
}

async fn call(client: &RpcClient, method: &str, params: Value) -> Value {
    client.call_as(method, params).await.unwrap()
}

async fn envelope(client: &RpcClient, id: &str, intent: Value) -> Value {
    let basis = call(client, "GetTaskCommandSnapshot", json!({"taskId":CHAT})).await;
    json!({"taskId":CHAT,"commandId":id,"actorId":"qa-user","clientId":"qa-preflight","clientSessionId":"qa-preflight","expectedTaskRevision":basis["snapshot"]["taskRevision"],"expectedBindingGeneration":basis["snapshot"]["bindingGeneration"],"expectedTurnId":basis["snapshot"]["activeTurnId"],"intent":intent})
}

async fn create(client: &RpcClient, engine: &EngineCore, provider: &str) {
    call(client, "Mutate", json!({"op":"createSpace","spaceId":"preflight-project","deviceId":engine.device_id,"path":"/tmp"})).await;
    let input = envelope(client, "create-preflight", json!({"kind":"createChat","spaceId":"preflight-project","cwd":"/tmp","config":{"harness":provider,"model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await;
    assert_eq!(
        call(client, "ExecuteTaskCommand", input).await["status"],
        "applied"
    );
}

async fn queue(client: &RpcClient, id: &str, text: &str) {
    call(
        client,
        "QueueMessage",
        json!({"chatId":CHAT,"messageId":id,"text":text,"holdForTurnEnd":true}),
    )
    .await;
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("expected durable/provider state");
}

fn row(engine: &EngineCore, id: &str) -> Value {
    serde_json::to_value(engine.doc_host.queued_message(CHAT, id).unwrap()).unwrap()
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("LUNA_PREFLIGHT_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_rejection_survives_restart_and_only_explicit_retry_delivers_once() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let uncertain = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    let engine = core(
        root.path(),
        seen.clone(),
        uncertain.clone(),
        interrupted.clone(),
    );
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine, "claude-code").await;
    queue(&client, "missing-account", "canonical unchanged prompt").await;
    let automatic = format!("queue-dispatch:{CHAT}:missing-account:1");
    wait_for(|| {
        engine
            .store
            .task_command_receipt(&automatic)
            .unwrap()
            .is_some_and(|r| {
                r.status != luna_proto::CommandReceiptStatus::Pending || r.reason.is_some()
            })
    })
    .await;
    let held = row(&engine, "missing-account");
    evidence(
        "local-before-restart",
        json!({"row":held,"receipt":engine.store.task_command_receipt(&automatic).unwrap(),"providerCalls":seen.lock().unwrap().len()}),
    );
    assert_eq!(held["holdReason"], "executionUnavailable");
    assert!(held["status"].is_null() || held["status"]["kind"] == "held");
    assert_eq!(seen.lock().unwrap().len(), 0);
    assert_eq!(
        call(
            &client,
            "GetTaskCommandReceipt",
            json!({"commandId":automatic})
        )
        .await["status"],
        "rejected"
    );
    engine.shutdown().await;
    drop(client);
    drop(engine);
    repair_account(root.path());
    let reopened = core(root.path(), seen.clone(), uncertain, interrupted);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    reopened.doc_host.open(CHAT).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(row(&reopened, "missing-account"), held);
    assert_eq!(
        seen.lock().unwrap().len(),
        0,
        "account repair/restart does not authorize replay"
    );
    for action in [
        luna_engine::doc_host::FinishQueueEditAction::Cancel,
        luna_engine::doc_host::FinishQueueEditAction::Commit,
    ] {
        let luna_engine::doc_host::BeginQueueEditOutcome::Acquired {
            lease_id,
            base_text_hash,
            ..
        } = reopened
            .doc_host
            .begin_queued_message_edit(CHAT, "missing-account", "qa-device", "qa-window")
            .await
            .unwrap()
        else {
            panic!("edit must acquire");
        };
        reopened
            .doc_host
            .finish_queued_message_edit(
                CHAT,
                "missing-account",
                &lease_id,
                action,
                Some("canonical unchanged prompt"),
                Some(&base_text_hash),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            row(&reopened, "missing-account")["holdReason"],
            "executionUnavailable",
            "edit/cancel must preserve explicit send requirement"
        );
        assert_eq!(seen.lock().unwrap().len(), 0);
    }
    let held = row(&reopened, "missing-account");
    let input = envelope(&client,"explicit-retry",json!({"kind":"sendQueuedNow","messageId":"missing-account","expectedQueueRevision":held["revision"]})).await;
    call(&client, "ExecuteTaskCommand", input.clone()).await;
    wait_for(|| seen.lock().unwrap().len() == 1 && row(&reopened, "missing-account").is_null())
        .await;
    call(&client, "ExecuteTaskCommand", input).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(*seen.lock().unwrap(), vec!["canonical unchanged prompt"]);
    let entries = reopened
        .doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.id == "missing-account")
            .count(),
        1
    );
    evidence(
        "explicit-retry",
        json!({"queue":row(&reopened,"missing-account"),"receipt":call(&client,"GetTaskCommandReceipt",json!({"commandId":"explicit-retry"})).await,"providerCalls":seen.lock().unwrap().len(),"canonicalUserEntries":1}),
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_send_now_preserves_the_running_turn() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let interrupted = Arc::new(AtomicBool::new(false));
    let engine = core(
        root.path(),
        seen.clone(),
        Arc::new(AtomicBool::new(false)),
        interrupted.clone(),
    );
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine, "mock").await;
    queue(&client, "active", "hold active turn").await;
    wait_for(|| seen.lock().unwrap().len() == 1 && row(&engine, "active").is_null()).await;
    let before = call(&client, "GetTaskCommandSnapshot", json!({"taskId":CHAT})).await;
    let mut config = engine
        .workspace
        .chat(CHAT)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    config.harness = HarnessId::ClaudeCode;
    engine.workspace.set_chat_config(CHAT, &config).unwrap();
    queue(&client, "blocked-send", "must remain queued").await;
    let input = envelope(
        &client,
        "send-missing-account",
        json!({"kind":"sendQueuedNow","messageId":"blocked-send","expectedQueueRevision":1}),
    )
    .await;
    let receipt = call(&client, "ExecuteTaskCommand", input).await;
    assert_eq!(receipt["status"], "rejected", "{receipt:#}");
    assert!(!interrupted.load(Ordering::SeqCst));
    assert!(engine.sessions.turn_in_flight(CHAT));
    assert_eq!(
        call(&client, "GetTaskCommandSnapshot", json!({"taskId":CHAT})).await["snapshot"]["activeTurnId"],
        before["snapshot"]["activeTurnId"]
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        row(&engine, "blocked-send")["holdReason"],
        "executionUnavailable"
    );
    evidence(
        "active-turn-preserved",
        json!({"receipt":receipt,"row":row(&engine,"blocked-send"),"providerCalls":1,"interrupted":false}),
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_entry_error_stays_unknown_across_restart() {
    let root = tempfile::tempdir().unwrap();
    repair_account(root.path());
    let seen = Arc::new(Mutex::new(vec![]));
    let uncertain = Arc::new(AtomicBool::new(true));
    let interrupted = Arc::new(AtomicBool::new(false));
    let engine = core(
        root.path(),
        seen.clone(),
        uncertain.clone(),
        interrupted.clone(),
    );
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine, "claude-code").await;
    queue(&client, "uncertain", "one ambiguous provider call").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    evidence(
        "unknown-observation",
        json!({"row":row(&engine,"uncertain"),"providerCalls":seen.lock().unwrap().len()}),
    );
    wait_for(|| seen.lock().unwrap().len() == 1 && !engine.sessions.turn_in_flight(CHAT)).await;
    assert!(matches!(
        row(&engine, "uncertain")["status"]["kind"].as_str(),
        Some("claimed" | "deliveryUnknown")
    ));
    assert_eq!(seen.lock().unwrap().len(), 1);
    let unknown = row(&engine, "uncertain");
    engine.shutdown().await;
    drop(client);
    drop(engine);
    uncertain.store(false, Ordering::SeqCst);
    let reopened = core(root.path(), seen.clone(), uncertain, interrupted);
    reopened.doc_host.open(CHAT).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = row(&reopened, "uncertain");
    assert!(matches!(
        after["status"]["kind"].as_str(),
        Some("claimed" | "deliveryUnknown")
    ));
    assert_eq!(after["id"], unknown["id"]);
    assert_eq!(after["revision"], unknown["revision"]);
    assert_eq!(seen.lock().unwrap().len(), 1);
    evidence(
        "unknown-after-restart",
        json!({"beforeRestart":unknown,"afterRestart":after,"providerCalls":1}),
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_receipt_transaction_failure_never_releases_a_claim_for_replay() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let uncertain = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    let engine = core(
        root.path(),
        seen.clone(),
        uncertain.clone(),
        interrupted.clone(),
    );
    let client = luna_rpc::memory_client(engine.rpc_service());
    create(&client, &engine, "claude-code").await;
    let db_path = root
        .path()
        .join("data/orgs/queue-preflight/queue-preflight/docs.sqlite3");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_preflight_rejection BEFORE INSERT ON task_command_events WHEN json_extract(NEW.receipt_json,'$.status')='rejected' BEGIN SELECT RAISE(ABORT,'forced preflight receipt failure'); END;").unwrap();
    queue(
        &client,
        "failed-receipt",
        "no dispatch after failed rejection commit",
    )
    .await;
    wait_for(|| {
        !row(&engine, "failed-receipt")["status"].is_null()
            && row(&engine, "failed-receipt")["status"]["kind"] != "held"
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let held = row(&engine, "failed-receipt");
    assert_ne!(
        held["holdReason"], "executionUnavailable",
        "uncommitted held projection cannot leak into the live document"
    );
    assert_eq!(seen.lock().unwrap().len(), 0);
    db.execute_batch("DROP TRIGGER fail_preflight_rejection")
        .unwrap();
    drop(db);
    engine.shutdown().await;
    drop(client);
    drop(engine);
    repair_account(root.path());
    let reopened = core(root.path(), seen.clone(), uncertain, interrupted);
    reopened.doc_host.open(CHAT).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(seen.lock().unwrap().len(), 0);
    assert_ne!(row(&reopened, "failed-receipt")["status"]["kind"], "held");
    evidence(
        "rejection-commit-failure",
        json!({"beforeRestart":held,"afterRestart":row(&reopened,"failed-receipt"),"providerCalls":0}),
    );
    reopened.shutdown().await;
}
