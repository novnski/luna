//! Clock-controlled RPC -> SQLite -> ordinary task command -> protocol-peer E2E.
//! Failure matrix: docs/design/harness-evolution/slice-5.md (written first).
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest};
use luna_rpc::RpcClient;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
const CHAT: &str = "a5555555-5555-4555-8555-555555555555";
const NOW: i64 = 2_051_222_400_000; // Future controlled clock; the real worker cannot claim these slots.
struct Peer(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Automation protocol peer"
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
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.0.lock().unwrap().push(r.prompt.clone());
        if r.prompt == "unknown" {
            return Err(HarnessError::Protocol("acceptance response lost".into()));
        }
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "fixture".into(),
                tools: vec![],
                cwd: r.cwd,
                session_id: "automation-native".into(),
                assistant_message_id: format!("reply-{}", uuid::Uuid::new_v4()),
            }),
            Ok(AgentEvent::TextDelta {
                text: "automation completed".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("automation-native".into()),
            }),
        ])
        .boxed())
    }
}
fn core(root: &std::path::Path, seen: Arc<Mutex<Vec<String>>>) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer(seen)));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "automation-e2e", "automation-e2e"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap()
}
async fn call(c: &RpcClient, m: &str, p: Value) -> Value {
    c.call_as(m, p).await.unwrap()
}
async fn create(c: &RpcClient, e: &EngineCore) {
    call(c,"Mutate",json!({"op":"createSpace","spaceId":"automation-project","deviceId":e.device_id,"path":"/tmp"})).await;
    call(c,"ExecuteTaskCommand",json!({"commandId":"create-automation-chat","taskId":CHAT,"expectedTaskRevision":0,"intent":{"kind":"createChat","spaceId":"automation-project","cwd":"/tmp","config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}}})).await;
}
fn save(e: &EngineCore, id: &str, prompt: &str, trigger: Value) -> Value {
    json!({"requestId":format!("save-{id}"),"automationId":id,"ownerHostId":e.device_id,"expectedRevision":0,"operation":{"kind":"save","definition":{"name":id,"prompt":prompt,"timezone":"UTC","target":{"kind":"existing","taskId":CHAT},"trigger":trigger,"maxLatenessMs":300000}}})
}
async fn mutate(c: &RpcClient, mut p: Value) -> Value {
    let preview = call(c, "PreviewAutomation", p.clone()).await;
    p["previewId"] = preview["previewId"].clone();
    call(c, "MutateAutomation", p).await
}
async fn runs(c: &RpcClient, id: &str) -> Value {
    call(c, "ListAutomationRuns", json!({"automationId":id})).await
}
async fn settle(e: &EngineCore, c: &RpcClient, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            e.rpc_service()
                .automation_tick_at(NOW + 1001)
                .await
                .unwrap();
            let r = runs(c, id).await;
            if r["items"][0]["state"] == "completed" {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
fn evidence(name: &str, v: Value) {
    if let Ok(p) = std::env::var("LUNA_AUTOMATION_EVIDENCE") {
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            std::path::Path::new(&p).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&v).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_occurrence_two_consumers_duplicate_mutation_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    let p = save(&e, "once", "once", json!({"kind":"once","atMs":NOW+1000}));
    let saved = mutate(&c, p.clone()).await;
    assert_eq!(saved["record"]["revision"], 1);
    assert_eq!(mutate(&c, p).await, saved);
    let (a, b) = tokio::join!(
        rpc.automation_tick_at(NOW + 1000),
        rpc.automation_tick_at(NOW + 1000)
    );
    a.unwrap();
    b.unwrap();
    let r = settle(&e, &c, "once").await;
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    evidence("one-occurrence", r.clone());
    e.shutdown().await;
    drop(c);
    drop(rpc);
    drop(e);
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    e.rpc_service()
        .automation_tick_at(NOW + 60000)
        .await
        .unwrap();
    assert_eq!(runs(&c, "once").await, r);
    assert_eq!(seen.lock().unwrap().len(), 1);
    e.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_revision_fence_and_sleep_coalescing() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    mutate(
        &c,
        save(
            &e,
            "interval",
            "interval",
            json!({"kind":"interval","anchorMs":NOW,"everyMinutes":1}),
        ),
    )
    .await;
    let paused=mutate(&c,json!({"requestId":"pause","automationId":"interval","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"pause"}})).await;
    e.rpc_service()
        .automation_tick_at(NOW + 600000)
        .await
        .unwrap();
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(paused["record"]["enabled"], false);
    mutate(
        &c,
        save(
            &e,
            "sleep",
            "sleep",
            json!({"kind":"interval","anchorMs":NOW,"everyMinutes":1}),
        ),
    )
    .await;
    e.rpc_service()
        .automation_tick_at(NOW + 600000)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    let r = runs(&c, "sleep").await;
    assert_eq!(r["items"][0]["scheduledAtMs"], NOW + 600000);
    assert!(r["items"][0]["coalesced"].as_u64().unwrap() > 0);
    evidence("pause-and-sleep", r);
    e.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_dispatch_survives_restart_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    mutate(
        &c,
        save(
            &e,
            "unknown",
            "unknown",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    e.rpc_service()
        .automation_tick_at(NOW + 1000)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    e.shutdown().await;
    drop(c);
    drop(e);
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    e.rpc_service()
        .automation_tick_at(NOW + 60000)
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    let r = runs(&c, "unknown").await;
    assert_eq!(r["items"][0]["state"], "unknown");
    let attention = call(&c, "GetAttention", json!({"targetDeviceId":e.device_id})).await;
    assert!(
        attention["needsAttention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "automationFailed")
    );
    assert!(!attention.to_string().contains("acceptance response lost"));
    mutate(&c,json!({"requestId":"delete-unknown","automationId":"unknown","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"delete"}})).await;
    let resolved = call(&c, "GetAttention", json!({"targetDeviceId":e.device_id})).await;
    assert!(
        !resolved["needsAttention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "automationFailed")
    );
    assert_eq!(runs(&c, "unknown").await["items"][0]["state"], "unknown");
    evidence(
        "automation-attention",
        json!({"unresolved":attention,"deleted":resolved}),
    );
    evidence("unknown-no-replay", r);
    e.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calendar_preview_skips_gap_and_chooses_first_fold() {
    let root = tempfile::tempdir().unwrap();
    let e = core(root.path(), Arc::new(Mutex::new(vec![])));
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    // The clock hook drives the same production recurrence implementation, not a parallel test algorithm.
    let gap = luna_engine::automation::preview_times(
        &luna_proto::AutomationTrigger::Calendar {
            hour: 2,
            minute: 30,
            weekdays: vec![],
        },
        "America/New_York",
        chrono::DateTime::parse_from_rfc3339("2035-03-10T12:00:00Z")
            .unwrap()
            .timestamp_millis(),
        3,
    )
    .unwrap();
    assert_eq!(
        gap[0],
        chrono::DateTime::parse_from_rfc3339("2035-03-12T06:30:00Z")
            .unwrap()
            .timestamp_millis()
    );
    let fold = luna_engine::automation::preview_times(
        &luna_proto::AutomationTrigger::Calendar {
            hour: 1,
            minute: 30,
            weekdays: vec![],
        },
        "America/New_York",
        chrono::DateTime::parse_from_rfc3339("2035-11-04T00:00:00Z")
            .unwrap()
            .timestamp_millis(),
        3,
    )
    .unwrap();
    assert_eq!(
        fold[0],
        chrono::DateTime::parse_from_rfc3339("2035-11-04T05:30:00Z")
            .unwrap()
            .timestamp_millis()
    );
    assert_eq!(
        fold[1],
        chrono::DateTime::parse_from_rfc3339("2035-11-05T06:30:00Z")
            .unwrap()
            .timestamp_millis()
    );
    evidence("dst", json!({"gap":gap,"fold":fold}));
    e.shutdown().await;
}

// PR producer failure cases are specified before implementation: complete bounded
// pages, same-second IDs, own replies, new head, unknown errors and a finite budget.
#[derive(Default)]
struct PrState {
    head: String,
    comments: usize,
    own_comment: bool,
    fail_comments: bool,
    checks_success: bool,
    paths: Vec<String>,
    statuses: usize,
}
struct Api(Arc<Mutex<PrState>>);
#[async_trait]
impl luna_engine::source_control::pr_watch::GithubReadApi for Api {
    async fn get(&self, _cwd: &std::path::Path, path: &str) -> Result<Value, String> {
        let mut s = self.0.lock().unwrap();
        s.paths.push(path.into());
        if path == "user" {
            return Ok(json!({"login":"fixture-viewer"}));
        }
        if path == "repos/fixture/repository/pulls/7" {
            return Ok(
                json!({"number":7,"state":"open","merged":false,"head":{"sha":s.head},"base":{"repo":{"full_name":"fixture/repository"}}}),
            );
        }
        if path.contains("check-runs") {
            return Ok(
                json!({"total_count":1,"check_runs":[{"id":91,"head_sha":s.head,"name":"build","status":if s.checks_success{"completed"}else{"in_progress"},"conclusion":if s.checks_success{json!("success")}else{Value::Null},"app":{"id":1}}]}),
            );
        }
        if path.contains("/status?") {
            let page: usize = path.rsplit("page=").next().unwrap().parse().unwrap();
            let rows: Vec<_> = ((page - 1) * 100..s.statuses.min(page * 100))
                .map(|i| json!({"context":format!("status-{i}"),"state":"success"}))
                .collect();
            return Ok(json!({"sha":s.head,"total_count":s.statuses,"statuses":rows}));
        }
        if path.contains("/issues/7/comments?") {
            if s.fail_comments {
                return Err("GitHub request failed (rate-limit reset unknown)".into());
            }
            let page: usize = path.rsplit("page=").next().unwrap().parse().unwrap();
            return Ok(Value::Array(((page-1)*100..s.comments.min(page*100)).map(|i|json!({"id":i+1,"body":format!("remark {i}"),"user":{"login":if s.own_comment&&i+1==s.comments{"fixture-viewer"}else{"reviewer"}},"created_at":"2035-01-01T00:00:00Z"})).collect()));
        }
        if path.contains("/pulls/7/comments?") || path.contains("/pulls/7/reviews?") {
            return Ok(json!([]));
        }
        panic!("unexpected endpoint {path}");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pr_watch_reads_complete_pages_and_wakes_once_per_meaningful_change() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    let state = Arc::new(Mutex::new(PrState {
        head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        comments: 100,
        checks_success: true,
        statuses: 101,
        ..Default::default()
    }));
    let api = Api(state.clone());
    mutate(&c,save(&e,"watch","Review new PR information",json!({"kind":"prWatch","repository":"fixture/repository","number":7,"pollMinutes":1,"wakeBudget":2}))).await;
    rpc.automation_tick_with_pr_api_at(NOW, &api).await.unwrap();
    assert!(seen.lock().unwrap().is_empty());
    assert!(
        e.store
            .automation("watch")
            .unwrap()
            .unwrap()
            .watch
            .unwrap()
            .last_error
            .is_none()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .paths
            .iter()
            .any(|p| p.ends_with("status?per_page=100&page=2"))
    );
    assert!(
        state
            .lock()
            .unwrap()
            .paths
            .iter()
            .any(|p| p.ends_with("comments?per_page=100&page=2"))
    );
    state.lock().unwrap().comments = 101;
    let (a, b) = tokio::join!(
        rpc.automation_tick_with_pr_api_at(NOW + 60000, &api),
        rpc.automation_tick_with_pr_api_at(NOW + 60000, &api)
    );
    a.unwrap();
    b.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    rpc.automation_tick_with_pr_api_at(NOW + 120000, &api)
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    {
        let mut s = state.lock().unwrap();
        s.comments = 102;
        s.own_comment = true;
    }
    rpc.automation_tick_with_pr_api_at(NOW + 180000, &api)
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    {
        let mut s = state.lock().unwrap();
        s.head = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
        s.checks_success = false;
    }
    rpc.automation_tick_with_pr_api_at(NOW + 240000, &api)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(seen.lock().unwrap().len(), 2);
    {
        let mut s = state.lock().unwrap();
        s.checks_success = true;
    }
    rpc.automation_tick_with_pr_api_at(NOW + 300000, &api)
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
    let saved = call(&c, "ListAutomations", json!({"automationId":"watch"})).await;
    assert_eq!(saved["items"][0]["watch"]["wakesUsed"], 2);
    assert!(saved["items"][0]["nextDueMs"].is_null());
    evidence(
        "pr-watch-budget",
        json!({"record":saved,"runs":runs(&c,"watch").await,"prompts":seen.lock().unwrap().clone()}),
    );
    e.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pr_watch_read_failure_is_unknown_and_does_not_advance_cursor() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    let state = Arc::new(Mutex::new(PrState {
        head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        comments: 1,
        ..Default::default()
    }));
    let api = Api(state.clone());
    mutate(&c,save(&e,"watch-error","Review changes",json!({"kind":"prWatch","repository":"fixture/repository","number":7,"pollMinutes":1,"wakeBudget":3}))).await;
    rpc.automation_tick_with_pr_api_at(NOW, &api).await.unwrap();
    let before = call(&c, "ListAutomations", json!({"automationId":"watch-error"})).await;
    state.lock().unwrap().fail_comments = true;
    rpc.automation_tick_with_pr_api_at(NOW + 60000, &api)
        .await
        .unwrap();
    let after = call(&c, "ListAutomations", json!({"automationId":"watch-error"})).await;
    assert_eq!(
        before["items"][0]["watch"]["wakeCursor"],
        after["items"][0]["watch"]["wakeCursor"]
    );
    assert!(after["items"][0]["watch"]["lastError"].is_string());
    assert!(seen.lock().unwrap().is_empty());
    mutate(&c,json!({"requestId":"pause-watch","automationId":"watch-error","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"pause"}})).await;
    let reads = state.lock().unwrap().paths.len();
    rpc.automation_tick_with_pr_api_at(NOW + 3600000, &api)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().paths.len(), reads);
    evidence("pr-watch-unknown-and-pause", after);
    e.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_agent_schedule_requires_exact_local_allow_once() {
    use luna_proto::ApprovalDecision;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        luna_harness::CodexHarness::new().with_executable(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/fake-codex.sh"),
        ),
    ));
    let accounts = AgentAccountsConfig {
        data_dir: root.path().join("accounts"),
        claude_config_dir: root.path().join("claude"),
        claude_config_file: root.path().join("claude.json"),
        codex_home: root.path().join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let e = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("data"),
            "automation-agent",
            "automation-agent",
        ),
        Arc::new(registry),
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap();
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    call(&c,"Mutate",json!({"op":"createSpace","spaceId":"automation-agent-project","deviceId":e.device_id,"path":project})).await;
    call(&c,"Mutate",json!({"op":"createChat","chatId":CHAT,"spaceId":"automation-agent-project","cwd":project,"config":{"harness":"codex","model":null,"sandbox":"workspace-write","permissionMode":"ask"}})).await;
    let (_, mut events) = e.sessions.subscribe(CHAT, 0).unwrap();
    call(&c,"QueueMessage",json!({"chatId":CHAT,"messageId":"agent-auto-request","text":format!("scenario:l5-automation:{CHAT}"),"holdForTurnEnd":true})).await;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut cards = vec![];
        loop {
            match events.recv().await.unwrap().event {
                AgentEvent::InputRequested {
                    request_id,
                    questions,
                } => {
                    assert_eq!(questions.len(), 1);
                    let q = &questions[0];
                    assert!(!q.approval.as_ref().unwrap().supports_task_allowance);
                    assert!(q.question.contains("Automation"));
                    // Generic provider controls cannot mint this engine-owned persistent grant.
                    assert!(
                        !e.sessions
                            .respond_approval(CHAT, &request_id, ApprovalDecision::AllowOnce)
                            .unwrap()
                    );
                    let decision = if cards.is_empty() {
                        ApprovalDecision::AllowOnce
                    } else {
                        ApprovalDecision::AllowTask
                    };
                    let outcome =
                        e.sessions
                            .respond_luna_local_ui_approval(CHAT, &request_id, decision);
                    assert_eq!(
                        outcome,
                        if cards.is_empty() {
                            luna_engine::sessions::LunaApprovalOutcome::AllowedOnce
                        } else {
                            luna_engine::sessions::LunaApprovalOutcome::Denied
                        }
                    );
                    assert_eq!(
                        e.sessions.respond_luna_local_ui_approval(
                            CHAT,
                            &request_id,
                            ApprovalDecision::AllowOnce
                        ),
                        luna_engine::sessions::LunaApprovalOutcome::Stale
                    );
                    cards.push(q.clone());
                }
                AgentEvent::Done { status, error, .. } => break (cards, status, error),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result.1, DoneStatus::Completed, "{:?}", result.2);
    assert_eq!(result.0.len(), 2);
    let rows = call(&c, "ListAutomations", json!({"taskId":CHAT})).await;
    assert_eq!(rows["items"].as_array().unwrap().len(), 1);
    assert_eq!(rows["items"][0]["id"], "agent-approved");
    assert_eq!(rows["items"][0]["revision"], 1);
    assert!(e.store.automation("agent-denied").unwrap().is_none());
    evidence("agent-allow-once", json!({"cards":result.0,"records":rows}));
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_prompt_is_not_starved_by_due_slow_pr_watches() {
    struct Stalled;
    #[async_trait]
    impl luna_engine::source_control::pr_watch::GithubReadApi for Stalled {
        async fn get(&self, _: &std::path::Path, _: &str) -> Result<Value, String> {
            std::future::pending().await
        }
    }
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    for i in 0..40 {
        mutate(&c,save(&e,&format!("slow-watch-{i:02}"),"Watch PR",json!({"kind":"prWatch","repository":"fixture/repository","number":7,"pollMinutes":1,"wakeBudget":2}))).await;
    }
    mutate(
        &c,
        save(
            &e,
            "prompt-after-watches",
            "not starved",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    let job = tokio::spawn(async move {
        rpc.automation_tick_with_pr_api_at(NOW + 1000, &Stalled)
            .await
    });
    let delivered = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    job.abort();
    let _ = job.await;
    assert!(delivered, "Slow PR reads starved the ordinary due prompt");
    evidence(
        "slow-pr-fairness",
        json!({"dueWatches":40,"promptCalls":seen.lock().unwrap().clone()}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_user_queue_and_pause_at_admission_prevent_scheduled_delivery() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    // Seed an already-held user row, as restored from a proven preflight rejection.
    // A normal QueueMessage intentionally releases the transient Stop pause.
    let held: luna_doc::QueuedMessage = serde_json::from_value(json!({"id":"held-human-work","text":"held human work","issuedBy":e.device_id,"issuedAt":NOW,"holdReason":"executionUnavailable","status":{"kind":"held"}})).unwrap();
    e.doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .push_queued(&held)
        .unwrap();
    mutate(
        &c,
        save(
            &e,
            "queue-busy",
            "must not jump queue",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    rpc.automation_tick_at(NOW + 1000).await.unwrap();
    assert!(
        seen.lock().unwrap().is_empty(),
        "Automation jumped a held user queue"
    );
    let skipped = runs(&c, "queue-busy").await;
    assert_eq!(skipped["items"][0]["state"], "skipped");
    e.doc_host
        .remove_queued_message(CHAT, "held-human-work")
        .await
        .unwrap();
    mutate(
        &c,
        save(
            &e,
            "pause-fence",
            "must not cross pause",
            json!({"kind":"once","atMs":NOW+2000}),
        ),
    )
    .await;
    let lane = e.doc_host.task_command_gate(CHAT).await;
    let worker = rpc.clone();
    let job = tokio::spawn(async move { worker.automation_tick_at(NOW + 2000).await });
    let admission = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let r = runs(&c, "pause-fence").await;
            if r["items"]
                .as_array()
                .unwrap()
                .first()
                .is_some_and(|r| r["state"] == "dispatching")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        admission.is_ok(),
        "admission state: {}; queue: {:?}",
        runs(&c, "pause-fence").await,
        e.doc_host.open(CHAT).unwrap().doc().read_queue().unwrap()
    );
    mutate(&c,json!({"requestId":"pause-admission","automationId":"pause-fence","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"pause"}})).await;
    drop(lane);
    job.await.unwrap().unwrap();
    assert!(seen.lock().unwrap().is_empty());
    evidence(
        "queue-and-pause",
        json!({"held":skipped,"paused":runs(&c,"pause-fence").await}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_conversation_keeps_approved_access_and_mutation_failure_is_atomic() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    let mut p = save(
        &e,
        "new-chat",
        "new bounded chat",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    p["operation"]["definition"]["target"] = json!({"kind":"newConversation","originTaskId":CHAT});
    mutate(&c, p).await;
    let mut config = e.workspace.chat(CHAT).unwrap().unwrap().config.unwrap();
    config.permission_mode = luna_proto::PermissionMode::Full;
    call(
        &c,
        "Mutate",
        json!({"op":"setChatConfig","chatId":CHAT,"config":config}),
    )
    .await;
    rpc.automation_tick_at(NOW + 1000).await.unwrap();
    let r = settle(&e, &c, "new-chat").await;
    let created = r["items"][0]["taskId"].as_str().unwrap().to_owned();
    assert_ne!(created, CHAT);
    assert_eq!(
        e.workspace
            .chat(&created)
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .permission_mode,
        luna_proto::PermissionMode::Ask
    );
    let db = rusqlite::Connection::open(
        root.path()
            .join("data/orgs/automation-e2e/automation-e2e/docs.sqlite3"),
    )
    .unwrap();
    db.execute_batch("CREATE TRIGGER fail_automation_receipt BEFORE INSERT ON automation_mutations BEGIN SELECT RAISE(ABORT,'forced automation receipt failure'); END;").unwrap();
    let mut p = save(
        &e,
        "rollback",
        "must be atomic",
        json!({"kind":"once","atMs":NOW+3000}),
    );
    p["previewId"] = call(&c, "PreviewAutomation", p.clone()).await["previewId"].clone();
    assert!(c.call_as::<Value>("MutateAutomation", p).await.is_err());
    assert!(e.store.automation("rollback").unwrap().is_none());
    assert!(
        e.store
            .automation_mutation_receipt("save-rollback")
            .unwrap()
            .is_none()
    );
    db.execute_batch("DROP TRIGGER fail_automation_receipt;")
        .unwrap();
    drop(db);
    e.shutdown().await;
    drop(c);
    drop(rpc);
    drop(e);
    let reopened = core(root.path(), seen.clone());
    reopened
        .rpc_service()
        .automation_tick_at(NOW + 2000)
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(reopened.workspace.chat(&created).unwrap().is_some());
    evidence(
        "new-chat-atomic",
        json!({"run":r,"createdTaskId":created,"providerCalls":1,"rolledBackMutation":true}),
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn previews_expire_and_archived_origins_never_run() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    let mut p = save(
        &e,
        "preview-fences",
        "private preview prompt",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    let preview = call(&c, "PreviewAutomation", p.clone()).await;
    assert!(preview["expiresAtMs"].as_i64().unwrap() > chrono::Utc::now().timestamp_millis());
    p["previewId"] = json!(format!(
        "0:{}",
        preview["previewId"]
            .as_str()
            .unwrap()
            .split_once(':')
            .unwrap()
            .1
    ));
    let expired = c
        .call_as::<Value>("MutateAutomation", p.clone())
        .await
        .unwrap_err();
    assert!(expired.to_string().contains("expired"));
    p["previewId"] = preview["previewId"].clone();
    p["operation"]["definition"]["prompt"] = json!("changed after review");
    assert!(
        c.call_as::<Value>("MutateAutomation", p.clone())
            .await
            .is_err()
    );
    let mut invalid = p.clone();
    invalid["operation"]["definition"]["timezone"] = json!("Never/A_Zone");
    assert!(
        c.call_as::<Value>("PreviewAutomation", invalid)
            .await
            .is_err()
    );
    assert!(luna_engine::automation::zoned_ms("America/New_York", 2035, 3, 11, 2, 30).is_err());
    assert!(luna_engine::automation::zoned_ms("America/New_York", 2035, 11, 4, 1, 30).is_err());
    mutate(&c, p).await;
    call(
        &c,
        "Mutate",
        json!({"op":"setChatArchived","chatId":CHAT,"archived":true}),
    )
    .await;
    rpc.automation_tick_at(NOW + 1000).await.unwrap();
    let ledger = runs(&c, "preview-fences").await;
    assert_eq!(ledger["items"][0]["state"], "skipped");
    assert!(e.workspace.chat(CHAT).unwrap().unwrap().archived);
    assert!(seen.lock().unwrap().is_empty());
    evidence(
        "preview-and-archive",
        json!({"expired":expired.to_string(),"ledger":ledger}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_reconciliation_advances_beyond_a_full_page_of_unknown_claims() {
    use luna_proto::{AutomationRun, AutomationRunState};
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    for i in 0..102 {
        let id = format!("old-claim-{i:03}");
        mutate(
            &c,
            save(&e, &id, "old claim", json!({"kind":"once","atMs":NOW+1000})),
        )
        .await;
        let record = e.store.automation(&id).unwrap().unwrap();
        let mut next = record.clone();
        next.next_due_ms = None;
        let run = AutomationRun {
            id: format!("{i:019}-old"),
            automation_id: id.clone(),
            owner_host_id: e.device_id.clone(),
            automation_revision: 1,
            scheduled_at_ms: NOW,
            claimed_at_ms: NOW,
            updated_at_ms: NOW,
            coalesced: 0,
            task_id: CHAT.into(),
            command_id: format!("old-command-{i}"),
            message_id: format!("old-message-{i}"),
            state: AutomationRunState::Claimed,
            detail: "Restored from an older boot".into(),
            boot_id: "prior-boot".into(),
        };
        assert!(
            e.store
                .claim_automation(&record, &next, &run, None)
                .unwrap()
        );
    }
    for _ in 0..3 {
        rpc.automation_tick_at(NOW + 1000).await.unwrap();
    }
    let first = call(&c, "ListAutomationRuns", json!({"limit":100})).await;
    let second = call(
        &c,
        "ListAutomationRuns",
        json!({"limit":100,"afterId":first["nextAfterId"]}),
    )
    .await;
    assert_eq!(
        first["items"].as_array().unwrap().len() + second["items"].as_array().unwrap().len(),
        102
    );
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["items"].as_array().unwrap())
            .all(|r| r["state"] == "unknown")
    );
    assert!(seen.lock().unwrap().is_empty());
    evidence(
        "reconciliation-pages",
        json!({"first":first,"second":second,"providerCalls":0}),
    );
    e.shutdown().await;
}

/// Opt-in read-only production boundary proof. No schedule or external write.
#[tokio::test]
#[ignore = "requires authenticated gh and LUNA_AUTOMATION_LIVE_PR=owner/repo#number"]
async fn live_github_observation_uses_real_bounded_read_api() {
    let selected = std::env::var("LUNA_AUTOMATION_LIVE_PR").unwrap();
    let (repository, number) = selected.rsplit_once('#').unwrap();
    let observation = luna_engine::source_control::pr_watch::read_pr_watch(
        &luna_engine::source_control::GitHubCli::new(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        repository,
        number.parse().unwrap(),
    )
    .await
    .unwrap();
    assert!(!observation.head_oid.is_empty());
    evidence(
        "live-github",
        json!({"repository":repository,"number":number,"headOid":observation.head_oid,"state":observation.state,"checks":observation.checks,"remarkIds":observation.remarks.keys().collect::<Vec<_>>(),"observedAtMs":chrono::Utc::now().timestamp_millis(),"externalWrites":0}),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn excessive_calendar_backlog_pauses_without_starving_other_due_work() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    mutate(
        &c,
        save(
            &e,
            "old-calendar",
            "must not run old work",
            json!({"kind":"calendar","hour":9,"minute":0,"weekdays":[]}),
        ),
    )
    .await;
    let previous = e.store.automation("old-calendar").unwrap().unwrap();
    let mut old = previous.clone();
    old.next_due_ms = Some(0);
    assert!(e.store.observe_automation(&previous, &old, None).unwrap());
    mutate(
        &c,
        save(
            &e,
            "after-old-calendar",
            "current work",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    rpc.automation_tick_at(NOW + 1000).await.unwrap();
    let current = settle(&e, &c, "after-old-calendar").await;
    let old = e.store.automation("old-calendar").unwrap().unwrap();
    let ledger = runs(&c, "old-calendar").await;
    assert!(!old.enabled);
    assert!(old.next_due_ms.is_none());
    assert_eq!(ledger["items"][0]["state"], "failed");
    assert_eq!(seen.lock().unwrap().as_slice(), ["current work"]);
    evidence(
        "old-calendar",
        json!({"paused":old,"failed":ledger,"current":current}),
    );
    e.shutdown().await;
}
