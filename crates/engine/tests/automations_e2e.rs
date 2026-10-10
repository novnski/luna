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
struct Peer {
    seen: Arc<Mutex<Vec<Value>>>,
    completion: Option<Arc<tokio::sync::Notify>>,
}
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
        self.seen
            .lock()
            .unwrap()
            .push(serde_json::to_value(&r).unwrap());
        if r.prompt == "unknown" {
            return Err(HarnessError::Protocol("acceptance response lost".into()));
        }
        let session_id = r
            .resume
            .clone()
            .unwrap_or_else(|| format!("automation-native-{}", uuid::Uuid::new_v4()));
        let held = matches!(r.prompt.as_str(), "hold archive" | "hold unknown");
        let before_acceptance = r.prompt == "hold before acceptance";
        let unknown = r.prompt == "hold unknown";
        let started = AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "fixture".into(),
            tools: vec![],
            cwd: r.cwd,
            session_id: session_id.clone(),
            assistant_message_id: format!("reply-{}", uuid::Uuid::new_v4()),
        };
        let mut events = vec![];
        if !unknown {
            events.push(Ok(started));
        }
        events.push(Ok(AgentEvent::TextDelta {
            text: "automation completed".into(),
        }));
        let done = AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some(session_id),
        };
        let completion = self.completion.clone();
        let ready = self.completion.clone();
        Ok(futures::stream::once(async move {
            if before_acceptance && let Some(ready) = ready {
                ready.notified().await;
            }
            events
        })
        .flat_map(futures::stream::iter)
        .chain(futures::stream::once(async move {
            if held && let Some(completion) = completion {
                completion.notified().await;
            }
            Ok(done)
        }))
        .boxed())
    }
}
fn core(root: &std::path::Path, seen: Arc<Mutex<Vec<Value>>>) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer {
        seen,
        completion: None,
    }));
    registered_core(root, registry, HarnessId::Mock)
}
fn registered_core(
    root: &std::path::Path,
    registry: HarnessRegistry,
    default_harness: HarnessId,
) -> EngineCore {
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
        default_harness,
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
    for (mode, cross_project) in [("ask", false), ("full", true)] {
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
        call(&c,"Mutate",json!({"op":"createChat","chatId":CHAT,"spaceId":"automation-agent-project","cwd":project,"config":{"harness":"codex","model":null,"sandbox":"workspace-write","permissionMode":mode}})).await;
        let target = if cross_project {
            let folder = root.path().join("destination");
            std::fs::create_dir(&folder).unwrap();
            call(&c,"Mutate",json!({"op":"createSpace","spaceId":"destination-project","deviceId":e.device_id,"path":folder})).await;
            let id = uuid::Uuid::new_v4().to_string();
            call(&c,"Mutate",json!({"op":"createChat","chatId":id,"spaceId":"destination-project","cwd":folder,"config":{"harness":"codex","model":null,"sandbox":"workspace-write","permissionMode":"full"}})).await;
            id
        } else {
            CHAT.into()
        };
        let (_, mut events) = e.sessions.subscribe(CHAT, 0).unwrap();
        call(&c,"QueueMessage",json!({"chatId":CHAT,"messageId":"agent-auto-request","text":format!("scenario:l5-automation:{target}"),"holdForTurnEnd":true})).await;
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
        if cross_project {
            assert_eq!(
                rows["items"][0]["authority"]["projectId"],
                "destination-project"
            );
            assert!(result.0[0].question.contains("destination-project"));
        }
        let parts = e
            .doc_host
            .open(CHAT)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .into_iter()
            .flat_map(|entry| entry.parts)
            .collect::<Vec<_>>();
        let mut linked_calls = 0;
        for part in &parts {
            if let luna_doc::MessagePart::Tool {
                call:
                    luna_proto::ToolCall::Mcp {
                        server,
                        tool,
                        input: Some(input),
                    },
                ..
            } = part
                && server == "luna"
                && tool == "automation_update"
            {
                linked_calls += 1;
                assert_eq!(input.as_object().unwrap().len(), 1);
                assert!(input.get("automationId").is_some());
            }
        }
        assert!(
            linked_calls > 0,
            "native MCP automation calls must retain their safe inline-card ID"
        );
        evidence(
            &format!("agent-allow-once-{mode}-{cross_project}"),
            json!({"cards":result.0,"records":rows,"parts":parts}),
        );
        e.shutdown().await;
    }
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
            automation_name: id.clone(),
            project_id: "automation-project".into(),
            notification_policy: Default::default(),
            finalized_at_ms: None,
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

// Project workflow acceptance: the real RPC/store/dispatch boundary is the
// single owner. Pre-fix evidence and the failure matrix are retained in the
// automations-workflow design and the 2026-10-09 review artifact.
async fn complete_at(e: &EngineCore, c: &RpcClient, id: &str, now: i64) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            e.rpc_service().automation_tick_at(now).await.unwrap();
            let ledger = runs(c, id).await;
            if ledger["items"]
                .as_array()
                .unwrap()
                .first()
                .is_some_and(|r| r["state"] == "completed")
            {
                let task = ledger["items"][0]["taskId"].as_str().unwrap();
                if !e.sessions.turn_in_flight(task) {
                    return ledger;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

async fn project_chat(c: &RpcClient, e: &EngineCore, root: &std::path::Path, name: &str) -> String {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    call(
        c,
        "Mutate",
        json!({"op":"createSpace","spaceId":name,"deviceId":e.device_id,"path":folder}),
    )
    .await;
    let task = uuid::Uuid::new_v4().to_string();
    call(c,"Mutate",json!({"op":"createChat","chatId":task,"spaceId":name,"cwd":folder,"config":{"harness":"mock","model":"fixture","reasoning":"high","sandbox":"workspace-write","permissionMode":"ask"}})).await;
    task
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_execution_is_independent_of_source_and_legacy_storage() {
    for new_chat in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let e = core(root.path(), seen.clone());
        let c = luna_rpc::memory_client(e.rpc_service());
        create(&c, &e).await;
        call(&c,"Mutate",json!({"op":"setChatConfig","chatId":CHAT,"config":{"harness":"mock","model":"saved-model","reasoning":"high","sandbox":"workspace-write","permissionMode":"ask"}})).await;
        let mut request = save(
            &e,
            "pinned",
            "scheduled pinned request",
            json!({"kind":"once","atMs":NOW+1000}),
        );
        if new_chat {
            request["operation"]["definition"]["target"] =
                json!({"kind":"newConversation","originTaskId":CHAT});
        }
        let saved = mutate(&c, request).await;
        // A real pre-change record has no execution/policy/completion fields.
        let mut legacy = saved["record"].clone();
        legacy.as_object_mut().unwrap().remove("completed");
        for field in [
            "execution",
            "sourceTaskId",
            "notificationPolicy",
            "archiveSuccessfulRuns",
        ] {
            legacy["definition"].as_object_mut().unwrap().remove(field);
        }
        let db = rusqlite::Connection::open(
            root.path()
                .join("data/orgs/automation-e2e/automation-e2e/docs.sqlite3"),
        )
        .unwrap();
        db.execute(
            "UPDATE automations SET record=?1 WHERE id='pinned'",
            [serde_json::to_vec(&legacy).unwrap()],
        )
        .unwrap();
        drop(db);
        call(&c,"Mutate",json!({"op":"setChatConfig","chatId":CHAT,"config":{"harness":"mock","model":"later-model","reasoning":"low","sandbox":"workspace-write","permissionMode":"ask"}})).await;
        let ledger = complete_at(&e, &c, "pinned", NOW + 1000).await;
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "saved-model");
        assert_eq!(requests[0]["reasoning"], "high");
        assert_eq!(
            e.workspace
                .chat(CHAT)
                .unwrap()
                .unwrap()
                .config
                .unwrap()
                .model
                .as_deref(),
            Some("later-model"),
            "the scheduled override must not replace the human chat's defaults"
        );
        evidence(
            &format!("workflow-pinned-{new_chat}"),
            json!({"saved":saved,"legacy":legacy,"requests":requests,"ledger":ledger}),
        );
        e.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_projects_block_runs_and_new_chats_outlive_their_source() {
    for new_chat in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let e = core(root.path(), seen.clone());
        let c = luna_rpc::memory_client(e.rpc_service());
        create(&c, &e).await;
        let mut request = save(
            &e,
            "removed",
            "must not run hidden",
            json!({"kind":"once","atMs":NOW+1000}),
        );
        if new_chat {
            request["operation"]["definition"]["target"] =
                json!({"kind":"newConversation","originTaskId":CHAT});
        }
        mutate(&c, request).await;
        call(
            &c,
            "Mutate",
            json!({"op":"removeSpace","spaceId":"automation-project"}),
        )
        .await;
        e.rpc_service()
            .automation_tick_at(NOW + 1000)
            .await
            .unwrap();
        assert!(seen.lock().unwrap().is_empty());
        let record = e.store.automation("removed").unwrap().unwrap();
        assert!(!record.enabled);
        assert_eq!(runs(&c, "removed").await["items"][0]["state"], "skipped");
        evidence(
            &format!("workflow-removed-{new_chat}"),
            json!({"record":record,"runs":runs(&c,"removed").await}),
        );
        e.shutdown().await;
    }
    for delete in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let e = core(root.path(), seen.clone());
        let c = luna_rpc::memory_client(e.rpc_service());
        create(&c, &e).await;
        let mut request = save(
            &e,
            "independent",
            "fresh conversation",
            json!({"kind":"once","atMs":NOW+1000}),
        );
        request["operation"]["definition"]["target"] =
            json!({"kind":"newConversation","originTaskId":CHAT});
        mutate(&c, request).await;
        call(
            &c,
            "Mutate",
            if delete {
                json!({"op":"deleteChat","chatId":CHAT})
            } else {
                json!({"op":"setChatArchived","chatId":CHAT,"archived":true})
            },
        )
        .await;
        let ledger = complete_at(&e, &c, "independent", NOW + 1000).await;
        let task = ledger["items"][0]["taskId"].as_str().unwrap();
        assert_ne!(task, CHAT);
        let chat = e.workspace.chat(task).unwrap().unwrap();
        assert_eq!(chat.space_id.as_deref(), Some("automation-project"));
        assert_eq!(chat.cwd.as_deref(), Some("/tmp"));
        evidence(
            &format!("workflow-source-gone-{delete}"),
            json!({"ledger":ledger,"chat":chat}),
        );
        e.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editing_paused_work_retargets_without_resuming_and_filters_intersect() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let other = project_chat(&c, &e, root.path(), "project-b").await;
    mutate(
        &c,
        save(
            &e,
            "move",
            "old prompt",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    mutate(&c,json!({"requestId":"pause-move","automationId":"move","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"pause"}})).await;
    let mut edit = save(
        &e,
        "move",
        "new prompt",
        json!({"kind":"once","atMs":NOW+61000}),
    );
    edit["requestId"] = json!("edit-move");
    edit["expectedRevision"] = json!(2);
    edit["operation"]["definition"]["name"] = json!("Renamed schedule");
    edit["operation"]["definition"]["target"] = json!({"kind":"existing","taskId":other});
    edit["operation"]["definition"]["execution"] =
        json!({"harness":"mock","model":"chosen","reasoning":"high"});
    let edited = mutate(&c, edit).await;
    assert_eq!(edited["record"]["enabled"], false);
    assert_eq!(edited["record"]["authority"]["projectId"], "project-b");
    assert_eq!(edited["record"]["definition"]["name"], "Renamed schedule");
    for params in [
        json!({"projectId":"automation-project"}),
        json!({"automationId":"move","projectId":"automation-project"}),
        json!({"automationId":"move","taskId":"unrelated-chat"}),
    ] {
        assert!(
            call(&c, "ListAutomations", params).await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        call(&c, "ListAutomations", json!({"projectId":"project-b"})).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    e.rpc_service()
        .automation_tick_at(NOW + 61000)
        .await
        .unwrap();
    assert!(seen.lock().unwrap().is_empty());
    mutate(&c,json!({"requestId":"resume-move","automationId":"move","ownerHostId":e.device_id,"expectedRevision":3,"operation":{"kind":"resume"}})).await;
    let ledger = complete_at(&e, &c, "move", NOW + 61000).await;
    assert_eq!(ledger["items"][0]["taskId"], other);
    assert_eq!(seen.lock().unwrap()[0]["model"], "chosen");
    assert_eq!(seen.lock().unwrap()[0]["prompt"], "new prompt");
    assert!(e.store.automation("move").unwrap().unwrap().completed);
    let mut definition =
        serde_json::to_value(e.store.automation("move").unwrap().unwrap().definition).unwrap();
    definition["name"] = json!("Finished schedule renamed");
    let renamed=mutate(&c,json!({"requestId":"rename-finished","automationId":"move","ownerHostId":e.device_id,"expectedRevision":4,"operation":{"kind":"save","definition":definition}})).await;
    assert_eq!(renamed["record"]["completed"], true);
    assert_eq!(renamed["record"]["enabled"], false);
    e.rpc_service()
        .automation_tick_at(NOW + 61001)
        .await
        .unwrap();
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "a metadata edit of finished work must not run it again"
    );
    assert!(
        call(
            &c,
            "ListAutomationRuns",
            json!({"automationId":"move","projectId":"automation-project"})
        )
        .await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    mutate(&c,json!({"requestId":"delete-move","automationId":"move","ownerHostId":e.device_id,"expectedRevision":5,"operation":{"kind":"delete"}})).await;
    assert!(
        call(&c, "ListAutomations", json!({"automationId":"move"})).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let deleted = call(
        &c,
        "ListAutomations",
        json!({"automationId":"move","includeDeleted":true,"projectId":"project-b"}),
    )
    .await;
    assert_eq!(deleted["items"].as_array().unwrap().len(), 1);
    assert_eq!(runs(&c, "move").await["items"].as_array().unwrap().len(), 1);
    evidence(
        "workflow-edit-lifecycle",
        json!({"edited":edited,"runs":ledger,"deleted":deleted}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_resolution_allows_only_separate_future_work() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    mutate(
        &c,
        save(
            &e,
            "unknown-future",
            "unknown",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    e.rpc_service()
        .automation_tick_at(NOW + 1000)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let called = seen.lock().unwrap().len() == 1;
            if called
                && !e.sessions.turn_in_flight(CHAT)
                && runs(&c, "unknown-future").await["items"][0]["state"] == "unknown"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let original = runs(&c, "unknown-future").await["items"][0].clone();
    e.shutdown().await;
    drop(c);
    drop(e);
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    let mut edit = save(
        &e,
        "unknown-future",
        "reviewed new work",
        json!({"kind":"once","atMs":NOW+61000}),
    );
    edit["requestId"] = json!("edit-unknown-future");
    edit["expectedRevision"] = json!(1);
    edit["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","projectId":"automation-project"});
    mutate(&c, edit).await;
    mutate(&c,json!({"requestId":"resolve-unknown-future","automationId":"unknown-future","ownerHostId":e.device_id,"expectedRevision":2,"operation":{"kind":"resolveUnknown","occurrenceId":original["id"]}})).await;
    let ledger = complete_at(&e, &c, "unknown-future", NOW + 61000).await;
    assert_eq!(ledger["items"][1]["state"], "resolved");
    assert_eq!(ledger["items"][1]["messageId"], original["messageId"]);
    assert_ne!(ledger["items"][0]["messageId"], original["messageId"]);
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(seen.lock().unwrap()[1]["prompt"], "reviewed new work");
    evidence(
        "workflow-unknown-resolved",
        json!({"original":original,"runs":ledger,"requests":seen.lock().unwrap().clone()}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_created_schedules_use_saved_provider_and_distinct_fresh_chats() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let mut request = save(
        &e,
        "project-only",
        "project scheduled work",
        json!({"kind":"interval","anchorMs":NOW+1000,"everyMinutes":1}),
    );
    request["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","projectId":"automation-project"});
    request["operation"]["definition"]["execution"] =
        json!({"harness":"mock","model":"fixture","reasoning":"high"});
    mutate(&c, request).await;
    complete_at(&e, &c, "project-only", NOW + 1000).await;
    let ledger = complete_at(&e, &c, "project-only", NOW + 61000).await;
    assert_eq!(ledger["items"].as_array().unwrap().len(), 2);
    let first = ledger["items"][0]["taskId"].as_str().unwrap();
    let second = ledger["items"][1]["taskId"].as_str().unwrap();
    assert_ne!(first, second);
    for id in [first, second] {
        let row = e.workspace.chat(id).unwrap().unwrap();
        assert_eq!(row.space_id.as_deref(), Some("automation-project"));
        assert_eq!(row.cwd.as_deref(), Some("/tmp"));
        assert_eq!(row.config.unwrap().harness, HarnessId::Mock);
    }
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|r| r["harness"] == "mock" && r["reasoning"] == "high")
    );
    evidence(
        "workflow-project-fresh-chats",
        json!({"runs":ledger,"requests":seen.lock().unwrap().clone()}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notifications_and_success_archive_follow_saved_policy() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let mut records = vec![];
    for (index, policy) in ["allRuns", "importantUpdates", "never"]
        .into_iter()
        .enumerate()
    {
        let id = format!("notify-{policy}");
        let at = NOW + 1000 + index as i64 * 60_000;
        let mut request = save(&e, &id, &id, json!({"kind":"once","atMs":at}));
        request["operation"]["definition"]["target"] =
            json!({"kind":"newConversation","originTaskId":CHAT});
        request["operation"]["definition"]["notificationPolicy"] = json!(policy);
        request["operation"]["definition"]["archiveSuccessfulRuns"] = json!(policy == "allRuns");
        mutate(&c, request).await;
        let ledger = complete_at(&e, &c, &id, at).await;
        let task = ledger["items"][0]["taskId"].as_str().unwrap();
        e.rpc_service().automation_tick_at(at + 1).await.unwrap();
        assert_eq!(
            e.workspace.chat(task).unwrap().unwrap().archived,
            policy == "allRuns"
        );
        let record = e.store.automation(&id).unwrap().unwrap();
        assert!(record.completed && !record.enabled);
        records.push(json!({"record":record,"runs":runs(&c,&id).await}));
    }
    let attention = call(&c, "GetAttention", json!({"targetDeviceId":e.device_id})).await;
    let text = attention.to_string();
    assert!(text.contains("automationCompleted"));
    let completions = attention["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "automationCompleted")
        .count();
    assert_eq!(completions, 3);
    assert_eq!(
        attention["recent"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "automationCompleted" && r["suppressNotification"] == false)
            .count(),
        1
    );
    assert!(!text.contains("\"kind\":\"taskCompleted\""));
    evidence(
        "workflow-notifications-archive",
        json!({"records":records,"attention":attention}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn muted_uncertainty_stays_actionable_and_repeated_skips_do_not_notify_again() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let mut request = save(
        &e,
        "muted-unknown",
        "unknown",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    request["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","originTaskId":CHAT});
    request["operation"]["definition"]["notificationPolicy"] = json!("never");
    mutate(&c, request).await;
    e.rpc_service()
        .automation_tick_at(NOW + 1000)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let called = !seen.lock().unwrap().is_empty();
            let ledger = runs(&c, "muted-unknown").await;
            let task = ledger["items"][0]["taskId"].as_str().unwrap();
            if called
                && !e.sessions.turn_in_flight(task)
                && ledger["items"][0]["state"] == "unknown"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let attention = call(&c, "GetAttention", json!({"targetDeviceId":e.device_id})).await;
    let blocked = attention["needsAttention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["target"]["automationId"] == "muted-unknown")
        .expect("muting must not hide an unresolved automation");
    assert_eq!(blocked["suppressNotification"], true);
    assert_eq!(blocked["target"]["projectId"], "automation-project");
    e.store
        .claim_attention_transitions("automation-policy-e2e", false)
        .unwrap();
    let held:luna_doc::QueuedMessage=serde_json::from_value(json!({"id":"held-for-automation","text":"existing human work","issuedBy":e.device_id,"issuedAt":NOW,"holdReason":"executionUnavailable","status":{"kind":"held"}})).unwrap();
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
            "busy-slots",
            "must not jump human work",
            json!({"kind":"interval","anchorMs":NOW+61000,"everyMinutes":1}),
        ),
    )
    .await;
    e.rpc_service()
        .automation_tick_at(NOW + 61000)
        .await
        .unwrap();
    call(&c, "GetAttention", json!({"targetDeviceId":e.device_id})).await;
    let first = e
        .store
        .claim_attention_transitions("automation-policy-e2e", true)
        .unwrap();
    assert_eq!(first.iter().filter(|t|matches!(&t.item.target,luna_proto::AttentionTarget::Automation{automation_id,..} if automation_id=="busy-slots")).count(),1);
    e.rpc_service()
        .automation_tick_at(NOW + 121000)
        .await
        .unwrap();
    let after = call(&c, "GetAttention", json!({"targetDeviceId":e.device_id})).await;
    let second = e
        .store
        .claim_attention_transitions("automation-policy-e2e", true)
        .unwrap();
    assert!(!second.iter().any(|t|matches!(&t.item.target,luna_proto::AttentionTarget::Automation{automation_id,..} if automation_id=="busy-slots")));
    assert_eq!(
        after["needsAttention"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["target"]["automationId"] == "busy-slots")
            .count(),
        1
    );
    evidence(
        "workflow-notification-suppression",
        json!({"muted":attention,"after":after,"firstTransitions":first,"secondTransitions":second}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_calendar_previews_validate_the_supported_rule_contract() {
    let root = tempfile::tempdir().unwrap();
    let e = core(root.path(), Arc::new(Mutex::new(vec![])));
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let ms = |text: &str| {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis()
    };
    let anchor = ms("2035-01-01T00:00:00Z");
    let cases = [
        (
            "FREQ=MONTHLY;BYDAY=1MO;BYHOUR=9;BYMINUTE=15",
            vec![
                "2035-01-01T07:15:00Z",
                "2035-02-05T07:15:00Z",
                "2035-03-05T07:15:00Z",
            ],
        ),
        (
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;BYHOUR=10;BYMINUTE=0",
            vec![
                "2035-01-01T08:00:00Z",
                "2035-01-03T08:00:00Z",
                "2035-01-15T08:00:00Z",
            ],
        ),
        (
            "FREQ=MONTHLY;BYMONTHDAY=-1;BYHOUR=8;BYMINUTE=0",
            vec![
                "2035-01-31T06:00:00Z",
                "2035-02-28T06:00:00Z",
                "2035-03-31T05:00:00Z",
            ],
        ),
        (
            "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29;BYHOUR=12;BYMINUTE=0",
            vec![
                "2036-02-29T10:00:00Z",
                "2040-02-29T10:00:00Z",
                "2044-02-29T10:00:00Z",
            ],
        ),
        (
            "FREQ=DAILY;BYHOUR=9;BYMINUTE=0;UNTIL=20350102T070000Z",
            vec!["2035-01-01T07:00:00Z", "2035-01-02T07:00:00Z"],
        ),
    ];
    let mut previews = vec![];
    for (i, (rule, expected)) in cases.into_iter().enumerate() {
        let mut request = save(
            &e,
            &format!("rule-{i}"),
            "calendar",
            json!({"kind":"rRule","rule":rule,"anchorMs":anchor}),
        );
        request["operation"]["definition"]["timezone"] = json!("Europe/Bucharest");
        let preview = call(&c, "PreviewAutomation", request).await;
        assert_eq!(
            preview["nextOccurrencesMs"],
            json!(expected.into_iter().map(ms).collect::<Vec<_>>())
        );
        previews.push(json!({"rule":rule,"preview":preview}));
    }
    for rule in [
        "FREQ=DAILY;COUNT=2",
        "FREQ=MONTHLY;BYDAY=-128MO",
        "FREQ=WEEKLY;BYMONTHDAY=1",
        "FREQ=DAILY;INTERVAL=0",
        "FREQ=MONTHLY;FREQ=DAILY",
        "FREQ=HOURLY;BYHOUR=9",
    ] {
        let request = save(
            &e,
            "invalid-rule",
            "invalid",
            json!({"kind":"rRule","rule":rule,"anchorMs":anchor}),
        );
        assert!(
            c.call_as::<Value>("PreviewAutomation", request)
                .await
                .is_err(),
            "accepted {rule}"
        );
    }
    evidence("workflow-custom-calendar", json!({"previews":previews}));
    e.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_schedule_reaches_the_real_opencode_http_sse_adapter() {
    // This exercises the actual adapter against its provider-specific 2.x peer,
    // not a generic mock relabeled as OpenCode and not real-provider evidence.
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("opencode-project");
    std::fs::create_dir(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        luna_harness::OpencodeHarness::new().with_executable(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/fake-opencode-board.py"),
        ),
    ));
    let e = registered_core(root.path(), registry, HarnessId::Opencode);
    let c = luna_rpc::memory_client(e.rpc_service());
    call(&c,"Mutate",json!({"op":"createSpace","spaceId":"opencode-project","deviceId":e.device_id,"path":project})).await;
    let mut request = save(
        &e,
        "opencode-native",
        "Complete this scheduled protocol probe",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    request["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","projectId":"opencode-project"});
    request["operation"]["definition"]["execution"] =
        json!({"harness":"opencode","model":"fake/board"});
    mutate(&c, request).await;
    let ledger = complete_at(&e, &c, "opencode-native", NOW + 1000).await;
    let task = ledger["items"][0]["taskId"].as_str().unwrap();
    let row = e.workspace.chat(task).unwrap().unwrap();
    assert_eq!(row.config.as_ref().unwrap().harness, HarnessId::Opencode);
    assert_eq!(row.space_id.as_deref(), Some("opencode-project"));
    let sessions = std::fs::read_dir(&project)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|f| {
            f.file_name()
                .to_string_lossy()
                .starts_with(".luna-opencode-board-")
                && f.path().extension().is_some_and(|x| x == "json")
        })
        .map(|f| serde_json::from_slice::<Value>(&std::fs::read(f.path()).unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["model"]["providerID"], "fake");
    assert_eq!(sessions[0]["model"]["id"], "board");
    evidence(
        "workflow-opencode-adapter",
        json!({"ledger":ledger,"chat":row,"sessions":sessions}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_archive_never_hides_a_queued_human_followup() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let completion = Arc::new(tokio::sync::Notify::new());
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer {
        seen: seen.clone(),
        completion: Some(completion.clone()),
    }));
    let e = registered_core(root.path(), registry, HarnessId::Mock);
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let mut request = save(
        &e,
        "archive-human",
        "hold archive",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    request["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","originTaskId":CHAT});
    request["operation"]["definition"]["archiveSuccessfulRuns"] = json!(true);
    mutate(&c, request).await;
    e.rpc_service()
        .automation_tick_at(NOW + 1000)
        .await
        .unwrap();
    let ledger = runs(&c, "archive-human").await;
    let task = ledger["items"][0]["taskId"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !seen.lock().unwrap().is_empty() && e.sessions.turn_in_flight(&task) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    call(&c,"QueueMessage",json!({"chatId":task,"messageId":"human-after-automation","text":"human follow-up","holdForTurnEnd":true})).await;
    completion.notify_one();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let called = seen.lock().unwrap().len() == 2;
            if called && !e.sessions.turn_in_flight(&task) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    e.rpc_service()
        .automation_tick_at(NOW + 1001)
        .await
        .unwrap();
    assert!(!e.workspace.chat(&task).unwrap().unwrap().archived);
    assert_eq!(
        e.doc_host
            .open(&task)
            .unwrap()
            .doc()
            .read_latest_user_entry()
            .unwrap()
            .unwrap()
            .id,
        "human-after-automation"
    );
    assert_eq!(seen.lock().unwrap()[1]["prompt"], "human follow-up");
    evidence(
        "workflow-archive-human",
        json!({"runs":runs(&c,"archive-human").await,"requests":seen.lock().unwrap().clone(),"chat":e.workspace.chat(&task).unwrap()}),
    );
    e.shutdown().await;
}

/// A scheduler tick may land before the native provider acknowledges a live
/// turn. An uncommitted Pending receipt is not an uncertain terminal outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_for_provider_acceptance_does_not_report_unknown_delivery() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let ready = Arc::new(tokio::sync::Notify::new());
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer {
        seen: seen.clone(),
        completion: Some(ready.clone()),
    }));
    let e = registered_core(root.path(), registry, HarnessId::Mock);
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    mutate(
        &c,
        save(
            &e,
            "pending-acceptance",
            "hold before acceptance",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    e.rpc_service()
        .automation_tick_at(NOW + 1000)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while seen.lock().unwrap().is_empty() || !e.sessions.turn_in_flight(CHAT) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = runs(&c, "pending-acceptance").await;
    let receipt = e
        .store
        .task_command_receipt(before["items"][0]["commandId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, luna_proto::CommandReceiptStatus::Pending);
    assert!(receipt.committed_at_ms.is_none());
    // Live startup must also remain pending beyond the grace period used to
    // reconcile a provider that has already stopped without acknowledging.
    tokio::time::sleep(Duration::from_millis(5_100)).await;
    for _ in 0..2 {
        e.rpc_service()
            .automation_tick_at(NOW + 1001)
            .await
            .unwrap();
    }
    let waiting = runs(&c, "pending-acceptance").await;
    evidence(
        "workflow-pending-acceptance",
        json!({"receipt":receipt,"waiting":waiting}),
    );
    assert_eq!(
        waiting["items"][0]["state"], "dispatching",
        "live uncommitted delivery must remain pending"
    );
    ready.notify_one();
    let completed = settle(&e, &c, "pending-acceptance").await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    evidence("workflow-pending-acceptance-completed", completed);
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unresolved_live_delivery_cannot_be_acknowledged_while_still_running() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let completion = Arc::new(tokio::sync::Notify::new());
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer {
        seen: seen.clone(),
        completion: Some(completion.clone()),
    }));
    let e = registered_core(root.path(), registry, HarnessId::Mock);
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    mutate(
        &c,
        save(
            &e,
            "live-unknown",
            "hold unknown",
            json!({"kind":"once","atMs":NOW+1000}),
        ),
    )
    .await;
    e.rpc_service()
        .automation_tick_at(NOW + 1000)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let started = !seen.lock().unwrap().is_empty();
            if started && e.sessions.turn_in_flight(CHAT) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let pending = runs(&c, "live-unknown").await;
    // Model the owning command boundary explicitly closing an ambiguous
    // outcome while the old provider stream still has live work.
    e.doc_host
        .mark_task_command_unknown(
            pending["items"][0]["commandId"].as_str().unwrap(),
            "Acceptance response was lost while the native turn remains live",
        )
        .unwrap();
    e.rpc_service()
        .automation_tick_at(NOW + 1001)
        .await
        .unwrap();
    assert_eq!(
        runs(&c, "live-unknown").await["items"][0]["state"],
        "unknown"
    );
    let id = runs(&c, "live-unknown").await["items"][0]["id"].clone();
    let request = json!({"requestId":"resolve-live","automationId":"live-unknown","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"resolveUnknown","occurrenceId":id}});
    let error = c
        .call_as::<Value>("PreviewAutomation", request)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("running turn"));
    completion.notify_one();
    e.shutdown().await;
    evidence(
        "workflow-live-unknown-refused",
        json!({"error":error.to_string()}),
    );
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
    assert_eq!(seen.lock().unwrap()[0]["prompt"], "current work");
    assert_eq!(seen.lock().unwrap().len(), 1);
    evidence(
        "old-calendar",
        json!({"paused":old,"failed":ledger,"current":current}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn equivalent_new_chat_target_keeps_its_saved_folder_after_source_deletion() {
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    let project = root.path().join("project");
    let folder = project.join("selected-subfolder");
    std::fs::create_dir_all(&folder).unwrap();
    call(&c,"Mutate",json!({"op":"createSpace","spaceId":"saved-folder-project","deviceId":e.device_id,"path":project})).await;
    call(&c,"Mutate",json!({"op":"createChat","chatId":CHAT,"spaceId":"saved-folder-project","cwd":folder,"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await;
    let mut request = save(
        &e,
        "saved-folder",
        "run in the approved folder",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    request["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","originTaskId":CHAT});
    let saved = mutate(&c, request).await;
    // An older stored definition did not serialize projectId on this target.
    // Keep that real migration input even though new saves now normalize it.
    let mut legacy = saved["record"].clone();
    legacy["definition"]["target"]
        .as_object_mut()
        .unwrap()
        .remove("projectId");
    let db = rusqlite::Connection::open(
        root.path()
            .join("data/orgs/automation-e2e/automation-e2e/docs.sqlite3"),
    )
    .unwrap();
    db.execute(
        "UPDATE automations SET record=?1 WHERE id='saved-folder'",
        [serde_json::to_vec(&legacy).unwrap()],
    )
    .unwrap();
    drop(db);
    call(&c, "Mutate", json!({"op":"deleteChat","chatId":CHAT})).await;
    let mut definition = saved["record"]["definition"].clone();
    definition["target"]["projectId"] = json!("saved-folder-project");
    definition["name"] = json!("Renamed without changing destination");
    let edited=mutate(&c,json!({"requestId":"normalize-saved-folder","automationId":"saved-folder","ownerHostId":e.device_id,"expectedRevision":1,"operation":{"kind":"save","definition":definition}})).await;
    let actual = edited["record"]["authority"]["cwd"].as_str().unwrap();
    evidence(
        "workflow-saved-folder",
        json!({"saved":saved,"edited":edited,"expectedFolder":folder}),
    );
    assert_eq!(
        std::fs::canonicalize(actual).unwrap(),
        folder.canonicalize().unwrap(),
        "adding the destination's explicit project ID must not reset the saved folder to the project root"
    );
    let run = complete_at(&e, &c, "saved-folder", NOW + 1000).await;
    assert_eq!(seen.lock().unwrap()[0]["cwd"], json!(folder));
    evidence("workflow-saved-folder-run", run);
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_invalid_origins_and_foreign_provider_contexts_are_refused_before_save() {
    let root = tempfile::tempdir().unwrap();
    let e = core(root.path(), Arc::new(Mutex::new(vec![])));
    let c = luna_rpc::memory_client(e.rpc_service());
    create(&c, &e).await;
    let mut wrong_origin = save(
        &e,
        "bad-origin",
        "should not silently use the project root",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    wrong_origin["operation"]["definition"]["target"] = json!({"kind":"newConversation","originTaskId":"missing-source","projectId":"automation-project"});
    let origin_error = c
        .call_as::<Value>("PreviewAutomation", wrong_origin)
        .await
        .unwrap_err();
    assert!(
        origin_error
            .to_string()
            .contains("conversation was deleted")
    );
    assert!(e.store.automation("bad-origin").unwrap().is_none());
    let mut wrong_provider = save(
        &e,
        "bad-provider",
        "no implicit provider context reset",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    wrong_provider["operation"]["definition"]["execution"] =
        json!({"harness":"opencode","model":"fake/board"});
    let provider_error = c
        .call_as::<Value>("PreviewAutomation", wrong_provider)
        .await
        .unwrap_err();
    assert!(
        provider_error
            .to_string()
            .contains("existing conversation keeps its provider")
    );
    assert!(e.store.automation("bad-provider").unwrap().is_none());
    evidence(
        "workflow-explicit-refusals",
        json!({"origin":origin_error.to_string(),"provider":provider_error.to_string()}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn historical_delivery_does_not_grant_agent_access_after_project_retargeting() {
    use luna_engine::cross_task_tools::{CrossTaskAuthority, CrossTaskTools};
    let root = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen);
    let rpc = e.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    create(&c, &e).await;
    let creator = uuid::Uuid::new_v4().to_string();
    call(&c,"Mutate",json!({"op":"createChat","chatId":creator,"spaceId":"automation-project","cwd":"/tmp","config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await;
    let destination = project_chat(&c, &e, root.path(), "private-project-b").await;
    let mut initial = save(
        &e,
        "scoped-history",
        "original visible request",
        json!({"kind":"once","atMs":NOW+1000}),
    );
    initial["operation"]["definition"]["sourceTaskId"] = json!(creator);
    mutate(&c, initial).await;
    complete_at(&e, &c, "scoped-history", NOW + 1000).await;
    let mut updated = save(
        &e,
        "scoped-history",
        "PRIVATE PROJECT B PROMPT",
        json!({"kind":"once","atMs":NOW+61000}),
    );
    updated["requestId"] = json!("retarget-private");
    updated["expectedRevision"] = json!(1);
    updated["operation"]["definition"]["target"] = json!({"kind":"existing","taskId":destination});
    mutate(&c, updated).await;
    let authority = |caller: &str| CrossTaskAuthority {
        caller_task_id: caller.into(),
        local_environment_id: e.device_id.clone(),
        visible_project_ids: ["automation-project".to_owned()].into_iter().collect(),
        allow_projectless: false,
        granted_scopes: ["threads.read".to_owned()].into_iter().collect(),
        explicit_user_request: false,
        delivery: luna_proto::RuntimeDelivery::CodexMcpTools,
    };
    let tools = CrossTaskTools::new(rpc);
    let cancel = tokio_util::sync::CancellationToken::new();
    let former_recipient = tools
        .invoke(
            &authority(CHAT),
            "list_automations",
            json!({"automationId":"scoped-history"}),
            &cancel,
        )
        .await
        .unwrap();
    assert!(former_recipient["items"].as_array().unwrap().is_empty());
    assert!(!former_recipient.to_string().contains("PRIVATE"));
    let creator_read = tools
        .invoke(
            &authority(&creator),
            "list_automations",
            json!({"automationId":"scoped-history"}),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        creator_read["items"][0]["definition"]["prompt"],
        "PRIVATE PROJECT B PROMPT"
    );
    evidence(
        "workflow-agent-project-visibility",
        json!({"formerRecipient":former_recipient,"creator":creator_read}),
    );
    e.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_worktree_runs_but_a_changed_checkout_is_not_reused() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let other = root.path().join("other");
    let worktree = root.path().join("linked");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&other).unwrap();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=LUNA QA",
                "-c",
                "user.email=qa@example.test",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    for path in [&repo, &other] {
        git(path, &["init", "-q"]);
        std::fs::write(path.join("README.md"), "Disposable scheduler checkout QA\n").unwrap();
        git(path, &["add", "README.md"]);
        git(path, &["commit", "-qm", "Fixture"]);
    }
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "codex/automation-test",
            worktree.to_str().unwrap(),
        ],
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let e = core(root.path(), seen.clone());
    let c = luna_rpc::memory_client(e.rpc_service());
    call(
        &c,
        "Mutate",
        json!({"op":"createSpace","spaceId":"git-project","deviceId":e.device_id,"path":repo}),
    )
    .await;
    call(&c,"Mutate",json!({"op":"createChat","chatId":CHAT,"spaceId":"git-project","cwd":worktree,"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}})).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if e.workspace
                .chat(CHAT)
                .unwrap()
                .unwrap()
                .checkout_id
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut request = save(
        &e,
        "checkout-proof",
        "work in the approved checkout",
        json!({"kind":"interval","anchorMs":NOW+1000,"everyMinutes":1}),
    );
    request["operation"]["definition"]["target"] =
        json!({"kind":"newConversation","originTaskId":CHAT});
    let saved = mutate(&c, request).await;
    assert!(saved["record"]["authority"]["checkoutId"].is_string());
    let first = complete_at(&e, &c, "checkout-proof", NOW + 1000).await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", other.join(".git").display()),
    )
    .unwrap();
    e.rpc_service()
        .automation_tick_at(NOW + 61000)
        .await
        .unwrap();
    let after = runs(&c, "checkout-proof").await;
    assert_eq!(after["items"][0]["state"], "skipped");
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(
        e.workspace
            .chat(after["items"][0]["taskId"].as_str().unwrap())
            .unwrap()
            .is_none()
    );
    evidence(
        "workflow-checkout-revalidation",
        json!({"saved":saved,"first":first,"after":after,"providerCalls":seen.lock().unwrap().len()}),
    );
    e.shutdown().await;
}
