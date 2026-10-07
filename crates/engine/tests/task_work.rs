//! RPC -> durable ownership/stop receipt -> live protocol peer E2E.
//! Failure matrix is specified before implementation in slice-3.md.
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest};
use luna_rpc::RpcClient;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
struct Peer(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Work protocol peer"
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
        r: RunRequest,
        c: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.0.lock().unwrap().push(r.prompt.clone());
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "fixture".into(),
                tools: vec![],
                cwd: r.cwd,
                session_id: r
                    .resume
                    .unwrap_or_else(|| format!("native-{}", uuid::Uuid::new_v4())),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "Working".into(),
            }),
        ])
        .chain(futures::stream::once(async move {
            c.interrupt.cancelled().await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            })
        }))
        .boxed())
    }
}
fn engine(root: &std::path::Path) -> EngineCore {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("luna_engine=info")
        .with_test_writer()
        .try_init();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer(Arc::new(Mutex::new(vec![])))));
    EngineCore::assemble_with_profile(
        EngineProfile::development(&root.join("data"), "work-qa", "work-qa"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .unwrap()
}
async fn call(c: &RpcClient, m: &str, p: Value) -> Value {
    c.call_as(m, p).await.unwrap()
}
async fn command(c: &RpcClient, t: &str, intent: Value) -> Value {
    let b = call(c, "GetTaskCommandSnapshot", json!({"taskId":t})).await;
    call(c,"ExecuteTaskCommand",json!({"taskId":t,"commandId":uuid::Uuid::new_v4().to_string(),"actorId":"qa","clientId":"qa","clientSessionId":"qa","expectedTaskRevision":b["snapshot"]["taskRevision"],"expectedBindingGeneration":b["snapshot"]["bindingGeneration"],"intent":intent})).await
}
async fn queue(c: &RpcClient, t: &str, id: &str) {
    call(
        c,
        "QueueMessage",
        json!({"chatId":t,"messageId":id,"text":"wait for work stop","holdForTurnEnd":true}),
    )
    .await;
}
async fn active(e: &EngineCore, t: &str) {
    for _ in 0..300 {
        if e.sessions.active_turn_id(t).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await
    }
    panic!(
        "no active turn {t}: status={:?} queue={:?} entries={:?}",
        e.sessions.session_status(t),
        e.doc_host.open(t).unwrap().doc().read_queue(),
        e.doc_host.open(t).unwrap().doc().read_entries()
    )
}
async fn child(e: &EngineCore, p: &str, key: &str) -> String {
    let r=e.rpc_service().seed_child_for_qa(p.into(),serde_json::from_value(json!({"prompt":"wait for work stop","idempotencyKey":key,"reportResults":true,"continueParent":false})).unwrap()).await.unwrap();
    serde_json::to_value(r).unwrap()["taskId"]
        .as_str()
        .unwrap()
        .into()
}
fn artifact(name: &str, v: &Value) {
    if let Ok(dir) = std::env::var("LUNA_WORK_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(v).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_ownership_stop_receipts_and_independent_child_survival() {
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path());
    let c = luna_rpc::memory_client(e.rpc_service());
    let p = uuid::Uuid::new_v4().to_string();
    e.workspace
        .create_space(
            "work-space",
            &e.device_id,
            tmp.path().to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    assert_eq!(command(&c,&p,json!({"kind":"createChat","spaceId":"work-space","cwd":tmp.path(),"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await["status"],"applied");
    call(&c,"SaveProjectCommands",json!({"projectId":"work-space","expectedActions":[],"actions":[{"id":"service","name":"Bounded service","command":"sleep 30","waitForSetup":false}]})).await;
    let service = call(
        &c,
        "RunProjectCommand",
        json!({"chatId":p,"actionId":"service","requestId":"service-1"}),
    )
    .await;
    assert_eq!(service["status"], "running");
    artifact("service-before", &service);
    queue(&c, &p, "parent-initial").await;
    active(&e, &p).await;
    let a = child(&e, &p, "first").await;
    active(&e, &a).await;
    let g = child(&e, &a, "grandchild").await;
    active(&e, &g).await;
    let view = call(&c, "GetTaskWork", json!({"taskId":p})).await;
    artifact("nested-before", &view);
    let owned: Vec<_> = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "childChat" && i["owned"] == true)
        .collect();
    assert_eq!(owned.len(), 2, "{view}");
    assert!(
        owned
            .iter()
            .all(|i| i["origin"]["ownerTurnId"] == "parent-initial")
    );
    let service_item = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "service")
        .unwrap();
    assert_eq!(service_item["blocksCompletion"], false);
    assert_eq!(service_item["canStop"], false);
    let excluded = call(
        &c,
        "PreviewStopTaskWork",
        json!({"taskId":p,"workIds":[service_item["id"]]}),
    )
    .await;
    assert!(excluded["items"].as_array().unwrap().is_empty());
    assert_eq!(excluded["excluded"].as_array().unwrap().len(), 1);
    let ids: Vec<_> = owned.iter().map(|i| i["id"].clone()).collect();
    let preview = call(&c, "PreviewStopTaskWork", json!({"taskId":p,"workIds":ids})).await;
    artifact("preview", &preview);
    // The same child chat gets independent work after preview. The old selection must be stale.
    e.sessions.interrupt(&a).await.unwrap();
    queue(&c, &a, "independent").await;
    active(&e, &a).await;
    assert!(
        c.call(
            "StopTaskWork",
            json!({"taskId":p,"workIds":ids,"previewId":preview["previewId"],"requestId":"stale"})
        )
        .await
        .is_err()
    );
    assert!(e.sessions.active_turn_id(&a).is_some());
    let next = call(&c, "GetTaskWork", json!({"taskId":p})).await;
    artifact("independent-before-stop", &next);
    let ids: Vec<_> = next["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["canStop"] == true)
        .map(|i| i["id"].clone())
        .collect();
    assert_eq!(ids.len(), 1, "{next}");
    let pre = call(&c, "PreviewStopTaskWork", json!({"taskId":p,"workIds":ids})).await;
    let args = json!({"taskId":p,"workIds":ids,"previewId":pre["previewId"],"requestId":"stop-grandchild"});
    let receipt = call(&c, "StopTaskWork", args.clone()).await;
    artifact("stop", &receipt);
    assert_eq!(receipt["results"][0]["status"], "stopped");
    assert_eq!(receipt, call(&c, "StopTaskWork", args.clone()).await);
    assert!(e.sessions.active_turn_id(&g).is_none());
    assert!(e.sessions.active_turn_id(&a).is_some());
    assert!(e.sessions.active_turn_id(&p).is_some());
    let page = call(&c, "GetTaskWork", json!({"taskId":p,"limit":1})).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["nextOffset"].is_number());
    artifact("page", &page);
    let services = call(&c, "ProjectCommandStatus", json!({"chatId":p})).await;
    assert_eq!(services["runs"][0]["status"], "running");
    artifact("service-survived", &services);
    call(
        &c,
        "StopProjectCommand",
        json!({"chatId":p,"runId":service["id"]}),
    )
    .await;
    e.sessions.interrupt(&a).await.unwrap();
    e.sessions.interrupt(&p).await.unwrap();
    e.shutdown().await;
}

struct NativePeer {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    outcome: Arc<std::sync::atomic::AtomicUsize>,
    events: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>>>,
}
#[async_trait]
impl Harness for NativePeer {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Native work protocol peer"
    }
    fn supports_steering(&self) -> bool {
        true
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
    fn supports_native_child_stop(&self) -> bool {
        true
    }
    async fn stop_native_child(
        &self,
        session: &str,
        tool: &str,
    ) -> Result<luna_harness::NativeTaskStop, HarnessError> {
        assert_eq!(session, "PRIVATE_NATIVE_WORK_SESSION");
        assert_eq!(
            tool,
            if self.outcome.load(std::sync::atomic::Ordering::SeqCst) == 3 {
                "PRIVATE_NATIVE_WORK_INNER"
            } else {
                "PRIVATE_NATIVE_WORK_TOOL"
            }
        );
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(
            match self.outcome.load(std::sync::atomic::Ordering::SeqCst) {
                0 | 3 => luna_harness::NativeTaskStop::Requested,
                1 => luna_harness::NativeTaskStop::Unknown("provider answer lost".into()),
                _ => luna_harness::NativeTaskStop::Rejected("already settled".into()),
            },
        )
    }
    async fn run(
        &self,
        r: RunRequest,
        c: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.events.lock().unwrap() = Some(tx);
        let initial = vec![
            AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCode,
                model: "fixture".into(),
                tools: vec![],
                cwd: r.cwd,
                session_id: "PRIVATE_NATIVE_WORK_SESSION".into(),
                assistant_message_id: "native-work-assistant".into(),
            },
            AgentEvent::ToolCall {
                id: "PRIVATE_NATIVE_WORK_TOOL".into(),
                call: luna_proto::ToolCall::Unknown {
                    name: "Agent: Background worker".into(),
                    input: None,
                },
            },
            AgentEvent::Subagent {
                parent_tool_use_id: "PRIVATE_NATIVE_WORK_TOOL".into(),
                event: Box::new(AgentEvent::ChildStatus {
                    status: luna_proto::ChildRuntimeStatus::Active,
                }),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("PRIVATE_NATIVE_WORK_SESSION".into()),
            },
        ];
        Ok(futures::stream::iter(initial.into_iter().map(Ok)).chain(futures::stream::unfold((rx,c),|(mut rx,c)|async move{
            tokio::select! { _=c.interrupt.cancelled()=>None, event=rx.recv()=>event.map(|event|(Ok(event),(rx,c))) }
        })).boxed())
    }
}
fn native_engine(
    root: &std::path::Path,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    outcome: Arc<std::sync::atomic::AtomicUsize>,
    events: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>>>,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(NativePeer {
        calls,
        outcome,
        events,
    }));
    let accounts = luna_engine::AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    std::fs::write(
        &accounts.claude_config_file,
        r#"{"oauthAccount":{"accountUuid":"work-fixture","emailAddress":"work@example.invalid"}}"#,
    )
    .unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "work-native", "work-native"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_stop_acknowledgement_unknown_restart_and_terminal_readback() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let outcome = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(None));
    let e = native_engine(tmp.path(), calls.clone(), outcome.clone(), events.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    let p = uuid::Uuid::new_v4().to_string();
    e.workspace
        .create_space(
            "native-work",
            &e.device_id,
            tmp.path().to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    assert_eq!(command(&c,&p,json!({"kind":"createChat","spaceId":"native-work","cwd":tmp.path(),"config":{"harness":"claude-code","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await["status"],"applied");
    queue(&c, &p, "native-initial").await;
    let mut view = Value::Null;
    for _ in 0..200 {
        view = call(&c, "GetTaskWork", json!({"taskId":p})).await;
        if view["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "nativeChild" && i["state"] == "running")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    artifact("native-running", &view);
    assert!(!view.to_string().contains("PRIVATE_NATIVE"));
    assert_eq!(view["blockingCount"], 1);
    let ids = vec![view["items"][0]["id"].clone()];
    let preview = call(&c, "PreviewStopTaskWork", json!({"taskId":p,"workIds":ids})).await;
    let args = json!({"taskId":p,"workIds":ids,"previewId":preview["previewId"],"requestId":"native-stop-ack"});
    let db = rusqlite::Connection::open(
        tmp.path()
            .join("data/orgs/work-native/work-native/docs.sqlite3"),
    )
    .unwrap();
    db.execute_batch("CREATE TRIGGER fail_work_intent BEFORE INSERT ON snapshots WHEN NEW.doc_id LIKE 'luna-work-stop:%' BEGIN SELECT RAISE(ABORT,'forced stop intent failure'); END;").unwrap();
    assert!(c.call("StopTaskWork", args.clone()).await.is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no provider I/O before durable intent"
    );
    db.execute_batch("DROP TRIGGER fail_work_intent").unwrap();
    let receipt = call(&c, "StopTaskWork", args.clone()).await;
    artifact("native-ack", &receipt);
    assert_eq!(receipt["results"][0]["status"], "requested");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(receipt, call(&c, "StopTaskWork", args.clone()).await);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let active = call(&c, "GetTaskWork", json!({"taskId":p})).await;
    assert_eq!(active["items"][0]["state"], "running");
    let mut wrong = args.clone();
    wrong["workIds"] = json!(["another-item"]);
    assert!(c.call("StopTaskWork", wrong).await.is_err());
    db.execute_batch("CREATE TRIGGER fail_work_completion BEFORE UPDATE ON snapshots WHEN NEW.doc_id LIKE 'luna-work-stop:%' AND json_extract(NEW.bytes,'$.receipt.results[0].status')='requested' BEGIN SELECT RAISE(ABORT,'forced stop completion failure'); END;").unwrap();
    let failed_args = json!({"taskId":p,"workIds":ids,"previewId":preview["previewId"],"requestId":"native-stop-commit-fault"});
    assert!(c.call("StopTaskWork", failed_args.clone()).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    db.execute_batch("DROP TRIGGER fail_work_completion")
        .unwrap();
    let saved_unknown = call(&c, "StopTaskWork", failed_args).await;
    assert_eq!(saved_unknown["results"][0]["status"], "unknown");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "lost commit must not replay provider stop"
    );
    artifact("native-commit-fault", &saved_unknown);
    // A separately requested stop loses its answer. Its ID is now permanently
    // uncertain, including after restart, without another provider call.
    outcome.store(1, Ordering::SeqCst);
    let unknown_args = json!({"taskId":p,"workIds":ids,"previewId":preview["previewId"],"requestId":"native-stop-unknown"});
    let unknown = call(&c, "StopTaskWork", unknown_args.clone()).await;
    artifact("native-unknown", &unknown);
    assert_eq!(unknown["results"][0]["status"], "unknown");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    events
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .send(AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_TOOL".into(),
            event: Box::new(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }),
        })
        .unwrap();
    let mut terminal = Value::Null;
    for _ in 0..200 {
        terminal = call(&c, "GetTaskWork", json!({"taskId":p})).await;
        if terminal["items"][0]["state"] == "stopped" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    artifact("native-terminal", &terminal);
    assert_eq!(terminal["items"][0]["state"], "stopped");
    assert_eq!(terminal["blockingCount"], 0);
    e.shutdown().await;
    drop(db);
    drop(c);
    drop(e);
    let reopened = native_engine(tmp.path(), calls.clone(), outcome, events);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    assert_eq!(unknown, call(&client, "StopTaskWork", unknown_args).await);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    reopened.shutdown().await;
}

struct WarmPeer {
    finish: Arc<Mutex<std::collections::HashMap<String, tokio::sync::mpsc::UnboundedSender<()>>>>,
}
#[async_trait]
impl Harness for WarmPeer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Warm ownership protocol peer"
    }
    fn supports_steering(&self) -> bool {
        true
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
        r: RunRequest,
        c: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let session = r
            .resume
            .unwrap_or_else(|| format!("warm-{}", uuid::Uuid::new_v4()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.finish.lock().unwrap().insert(session.clone(), tx);
        let initial = vec![
            AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "fixture".into(),
                tools: vec![],
                cwd: r.cwd,
                session_id: session.clone(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            },
            AgentEvent::TextDelta {
                text: "Initial work".into(),
            },
        ];
        Ok(futures::stream::iter(initial.into_iter().map(Ok)).chain(futures::stream::unfold((rx,c,session),|(mut rx,mut c,session)|async move{
            let event=tokio::select!{
                _=c.interrupt.cancelled()=>return None,
                finish=rx.recv()=>{finish?;AgentEvent::Done{status:DoneStatus::Completed,result:None,error:None,session_id:Some(session.clone())}},
                steer=c.steering.recv()=>{let mut steer=steer?;if let Some(ack)=steer.acceptance.take(){let _=ack.send(luna_harness::SteerAcceptance::Accepted);}AgentEvent::Steered{user_message_id:steer.message_id,assistant_message_id:None,next_assistant_message_id:Some(uuid::Uuid::new_v4().to_string())}},
            };Some((Ok(event),(rx,c,session)))
        })).boxed())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn warm_child_reuse_does_not_inherit_earlier_stop_or_new_child_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let finish = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(WarmPeer {
        finish: finish.clone(),
    }));
    let e = EngineCore::assemble_with_profile(
        EngineProfile::development(&tmp.path().join("data"), "work-warm", "work-warm"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let c = luna_rpc::memory_client(e.rpc_service());
    let p = uuid::Uuid::new_v4().to_string();
    e.workspace
        .create_space(
            "work-warm",
            &e.device_id,
            tmp.path().to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    assert_eq!(command(&c,&p,json!({"kind":"createChat","spaceId":"work-warm","cwd":tmp.path(),"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await["status"],"applied");
    queue(&c, &p, "parent-warm").await;
    active(&e, &p).await;
    let a = child(&e, &p, "warm-child").await;
    active(&e, &a).await;
    let old_run = e.sessions.active_turn_id(&a).unwrap();
    let g = child(&e, &a, "initial-grandchild").await;
    active(&e, &g).await;
    let first = call(&c, "GetTaskWork", json!({"taskId":p})).await;
    let id = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["taskId"] == a && i["kind"] == "childChat")
        .unwrap()["id"]
        .clone();
    let old = call(
        &c,
        "PreviewStopTaskWork",
        json!({"taskId":p,"workIds":[id]}),
    )
    .await;
    let native = e
        .task_identity
        .view(&a)
        .unwrap()
        .current_binding
        .unwrap()
        .provider_task_id;
    finish.lock().unwrap()[&native].send(()).unwrap();
    for _ in 0..200 {
        if e.sessions.active_turn_id(&a).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(e.sessions.active_turn_id(&a).is_none());
    // Creating from an idle warm child is a new independent launch, even
    // though that native process originally ran a delegated turn.
    let later = child(&e, &a, "idle-independent-child").await;
    active(&e, &later).await;
    assert_eq!(
        e.workspace
            .chat(&later)
            .unwrap()
            .unwrap()
            .child_thread
            .unwrap()
            .work_origin
            .unwrap()
            .owner_task_id,
        a
    );
    queue(&c, &a, "warm-independent").await;
    active(&e, &a).await;
    assert_eq!(
        e.sessions.active_turn_id(&a).as_deref(),
        Some(old_run.as_str()),
        "must really reuse the same run"
    );
    assert!(c.call("StopTaskWork",json!({"taskId":p,"workIds":[id],"previewId":old["previewId"],"requestId":"stale-warm-stop"})).await.is_err());
    assert_eq!(
        e.sessions.active_turn_id(&a).as_deref(),
        Some(old_run.as_str())
    );
    let view = call(&c, "GetTaskWork", json!({"taskId":p})).await;
    artifact("warm-independent-survived", &view);
    assert!(
        view["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["taskId"] == g && i["owned"] == true && i["canStop"] == true)
    );
    assert!(
        view["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["taskId"] == a)
            .all(|i| i["canStop"] == false)
    );
    assert!(
        view["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["taskId"] != later)
    );
    for task in [&g, &later, &a, &p] {
        e.sessions.interrupt(task).await.unwrap();
    }
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_native_child_and_shell_remain_owned_after_intermediate_completion() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(None));
    let e = native_engine(
        tmp.path(),
        calls.clone(),
        Arc::new(AtomicUsize::new(3)),
        events.clone(),
    );
    let c = luna_rpc::memory_client(e.rpc_service());
    let p = uuid::Uuid::new_v4().to_string();
    e.workspace
        .create_space(
            "nested-native",
            &e.device_id,
            tmp.path().to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    assert_eq!(command(&c, &p, json!({"kind":"createChat","spaceId":"nested-native","cwd":tmp.path(),"config":{"harness":"claude-code","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await["status"], "applied");
    queue(&c, &p, "nested-initial").await;
    for _ in 0..200 {
        if events.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let nested = vec![
        AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_TOOL".into(),
            event: Box::new(AgentEvent::ToolCall {
                id: "PRIVATE_NATIVE_WORK_INNER".into(),
                call: luna_proto::ToolCall::Unknown {
                    name: "Agent: Nested worker".into(),
                    input: None,
                },
            }),
        },
        AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_INNER".into(),
            event: Box::new(AgentEvent::ChildStatus {
                status: luna_proto::ChildRuntimeStatus::Active,
            }),
        },
        AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_TOOL".into(),
            event: Box::new(AgentEvent::ToolCall {
                id: "PRIVATE_NATIVE_SHELL".into(),
                call: luna_proto::ToolCall::Exec {
                    command: "sleep 30".into(),
                },
            }),
        },
        AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_TOOL".into(),
            event: Box::new(AgentEvent::ToolResult {
                id: "PRIVATE_NATIVE_SHELL".into(),
                is_error: false,
                output: Some("Background shell started".into()),
                diff: None,
            }),
        },
        AgentEvent::NativeTaskChanged {
            task: Box::new(luna_proto::NativeTaskSnapshot {
                task_id: "PRIVATE_SHELL_TASK".into(),
                kind: luna_proto::NativeTaskKind::Shell,
                provider_session_id: Some("PRIVATE_NATIVE_WORK_SESSION".into()),
                tool_use_id: Some("PRIVATE_NATIVE_SHELL".into()),
                status: luna_proto::NativeTaskStatus::Running,
                ..Default::default()
            }),
        },
        AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_TOOL".into(),
            event: Box::new(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            }),
        },
    ];
    for event in nested {
        events
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(event)
            .unwrap();
    }
    let mut view = Value::Null;
    for _ in 0..200 {
        view = call(&c, "GetTaskWork", json!({"taskId":p})).await;
        if view["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "nativeChild" && i["state"] == "completed")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    artifact("nested-native-before", &view);
    assert_eq!(view["blockingCount"], 2, "{view}");
    let items = view["items"].as_array().unwrap();
    assert_eq!(
        items.iter().filter(|i| i["kind"] == "shell").count(),
        1,
        "shell must not duplicate its launching chip"
    );
    assert!(items.iter().all(|i| i["owned"] == true));
    let inner = items
        .iter()
        .find(|i| i["kind"] == "nativeChild" && i["state"] == "running")
        .unwrap();
    assert_eq!(inner["canStop"], true);
    let ids = vec![inner["id"].clone()];
    let preview = call(&c, "PreviewStopTaskWork", json!({"taskId":p,"workIds":ids})).await;
    let receipt = call(&c, "StopTaskWork", json!({"taskId":p,"workIds":ids,"previewId":preview["previewId"],"requestId":"nested-native-stop"})).await;
    assert_eq!(receipt["results"][0]["status"], "requested");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    events
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .send(AgentEvent::Subagent {
            parent_tool_use_id: "PRIVATE_NATIVE_WORK_INNER".into(),
            event: Box::new(AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }),
        })
        .unwrap();
    for _ in 0..200 {
        view = call(&c, "GetTaskWork", json!({"taskId":p})).await;
        if view["blockingCount"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(view["blockingCount"], 1);
    artifact("nested-native-child-stopped-shell-running", &view);
    // A restored historical row is not a fresh native observation, even
    // when this resumed provider has the same session ID.
    e.doc_host
        .open(&p)
        .unwrap()
        .doc()
        .upsert_native_task(&luna_proto::NativeTaskSnapshot {
            task_id: "PRIVATE_HISTORICAL_SHELL".into(),
            name: Some("Historical shell".into()),
            kind: luna_proto::NativeTaskKind::Shell,
            provider_session_id: Some("PRIVATE_NATIVE_WORK_SESSION".into()),
            tool_use_id: Some("PRIVATE_NATIVE_SHELL".into()),
            status: luna_proto::NativeTaskStatus::Running,
            ..Default::default()
        })
        .unwrap();
    let stale = call(&c, "GetTaskWork", json!({"taskId":p})).await;
    let historical = stale["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["title"] == "Historical shell")
        .unwrap();
    assert_eq!(historical["state"], "unknown");
    assert_eq!(historical["canStop"], false);
    artifact("historical-shell-on-live-resume", &stale);
    e.sessions.interrupt(&p).await.unwrap();
    e.shutdown().await;
}
