//! PB-0 board store E2E: real engine over the in-memory RPC transport →
//! the `Board*` dispatcher → SQLite (migration v19) → restart, backup and
//! restore. Contract: docs/design/project-board/pb0-contract.md.
//!
//! Failure cases, written before the store implementation:
//!
//! 1. Saving a ticket creates a chat, sends a prompt or starts a provider
//!    run (Board, normal-chat "save for later", and source provenance must
//!    all create zero chats/launches; the source chat gains only a Source
//!    link, never a working badge).
//! 2. A retried create with the same idempotency key (sequential, or two
//!    windows concurrently) creates a second ticket, a second Created event
//!    or a different receipt; the same key with different content is
//!    silently accepted instead of `idempotencyConflict`.
//! 3. A stale writer overwrites a field changed after its expected revision
//!    (must be `staleRevision` carrying the current state), or a stale
//!    writer editing an unrelated field is rejected instead of merged; a
//!    no-op edit produces an event or a revision bump.
//! 4. Restart loses tickets, keys, order, receipts or events, or reorders
//!    the board.
//! 5. Keys collide inside a project, are reused after archive/restart, are
//!    derived from the title, or two projects on one host share a prefix.
//! 6. The All-projects view mixes project scope (an item loses its own
//!    project/key, a project-scoped list leaks another project's tickets),
//!    or creating from All projects guesses a project.
//! 7. Archive deletes or detaches a ticket (it must only hide it: links,
//!    events and its column/rank survive; unarchive restores it in place).
//! 8. Backup → restore drops board tables, counters, receipts or events, or
//!    keeps a ticket created after the backup point.
//! 9. Reordering within a column changes the revision, starts/stops/sends
//!    anything, creates an execution intent, or emits more than one
//!    non-activity Reordered event; a retried reorder moves twice.
//! 10. Invalid input (empty/multi-line title, unknown project, unknown
//!     ticket, bad neighbor) panics or half-writes instead of a structured
//!     error with no side effect.
//! 11. A mutation is not visible to a `BoardWatch` subscriber as exactly one
//!     canonical journal event (UI and agents disagree after a change).
//! 12. A board method in the contract is not routed by the dispatcher
//!     (`unknown method`), or a slice operation panics instead of returning
//!     a structured error.
//! 13. Upgrading a profile created before the board (schema 18) loses
//!     existing data or fails to add the board tables.
//! 14. (Integration fix round, written before the dispatcher change.) A debug
//!     build overflows a 2 MiB tokio worker stack on the RPC path of the
//!     heaviest board operations (`BoardStart`, `BoardConvertChat`,
//!     `BoardMove`, `BoardReviewStart`) because the dispatcher inlines every
//!     arm's callee future into one poll frame; the process aborts before any
//!     board logic answers.
//!
//! Every scenario writes a JSON manifest (steps, expected/observed, SHA-256
//! of snapshots) under `target/qa/board/store/`.
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::backup::{BackupService, IsolatedRoot, RecoveryKey};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, BoardError, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest,
};
use luna_rpc::{RpcClient, RpcError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Records every provider run; the board store must never cause one.
struct Peer(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Board store protocol peer"
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
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.0.lock().unwrap().push(request.prompt.clone());
        Ok(futures::stream::iter(vec![Ok(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        })])
        .boxed())
    }
}

struct Fixture {
    _parent: tempfile::TempDir,
    root: PathBuf,
    backups: PathBuf,
    projects: PathBuf,
    seen: Arc<Mutex<Vec<String>>>,
}

fn fixture() -> Fixture {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("synthetic-luna");
    let backups = parent.path().join("backups");
    let projects = parent.path().join("projects");
    for dir in [&root, &backups, &projects] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        root.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
    std::fs::write(
        backups.join(".luna-l17-isolated-backups"),
        "L17 synthetic or copy-isolated backup destination\n",
    )
    .unwrap();
    Fixture {
        _parent: parent,
        root,
        backups,
        projects,
        seen: Arc::new(Mutex::new(Vec::new())),
    }
}

fn boot(f: &Fixture) -> (EngineCore, RpcClient) {
    boot_at(f, &f.root)
}

fn boot_at(f: &Fixture, root: &Path) -> (EngineCore, RpcClient) {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer(f.seen.clone())));
    let accounts_root = f.root.parent().unwrap().join("provider-fixtures");
    let accounts = AgentAccountsConfig {
        data_dir: accounts_root.join("accounts"),
        claude_config_dir: accounts_root.join("claude"),
        claude_config_file: accounts_root.join("claude.json"),
        codex_home: accounts_root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(root).unwrap(),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    (core, client)
}

async fn stop(core: EngineCore, client: RpcClient) {
    core.shutdown().await;
    drop(client);
    drop(core);
}

async fn call(c: &RpcClient, method: &str, params: Value) -> Result<Value, RpcError> {
    c.call_as::<Value>(method, params).await
}

async fn ok(c: &RpcClient, method: &str, params: Value) -> Value {
    match call(c, method, params.clone()).await {
        Ok(value) => value,
        Err(error) => panic!("{method} {params} failed: {error}"),
    }
}

fn board_error(error: RpcError) -> BoardError {
    match error {
        RpcError::Failed(message) => BoardError::from_rpc_message(&message)
            .unwrap_or_else(|| panic!("not a board error: {message}")),
        other => panic!("unexpected transport error: {other}"),
    }
}

async fn refused(c: &RpcClient, method: &str, params: Value) -> BoardError {
    match call(c, method, params.clone()).await {
        Ok(value) => panic!("{method} {params} unexpectedly succeeded: {value}"),
        Err(error) => board_error(error),
    }
}

async fn space(c: &RpcClient, core: &EngineCore, f: &Fixture, id: &str, folder: &str) -> String {
    let path = f.projects.join(folder);
    std::fs::create_dir_all(&path).unwrap();
    let path = path.to_string_lossy().to_string();
    ok(
        c,
        "Mutate",
        json!({"op":"createSpace","spaceId":id,"deviceId":core.device_id,"path":path}),
    )
    .await;
    path
}

async fn chat(c: &RpcClient, project: &str, task_id: &str, cwd: &str) {
    ok(c, "ExecuteTaskCommand", json!({"commandId":format!("create-{task_id}"),"taskId":task_id,"expectedTaskRevision":0,"intent":{"kind":"createChat","spaceId":project,"cwd":cwd,"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}}})).await;
}

async fn chats(c: &RpcClient) -> Vec<Value> {
    let mut watch = c.subscribe("WatchChats", json!({})).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap();
    first.as_array().cloned().unwrap_or_default()
}

fn create_request(request_id: &str, project: &str, title: &str) -> Value {
    json!({"requestId":request_id,"projectId":project,"title":title,"description":format!("Brief for {title}")})
}

async fn create(c: &RpcClient, request_id: &str, project: &str, title: &str) -> Value {
    ok(c, "BoardCreate", create_request(request_id, project, title)).await
}

async fn list(c: &RpcClient, scope: Value, extra: Value) -> Value {
    let mut params = json!({ "scope": scope });
    if let Value::Object(extra) = extra {
        params.as_object_mut().unwrap().extend(extra);
    }
    ok(c, "BoardList", params).await
}

fn project(id: &str) -> Value {
    json!({"kind":"project","projectId":id})
}

fn all() -> Value {
    json!({"kind":"allProjects"})
}

fn keys(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|view| view["item"]["key"].as_str().unwrap().to_owned())
        .collect()
}

async fn events(c: &RpcClient, item: &str) -> Vec<Value> {
    ok(c, "BoardEvents", json!({"workItemId":item,"limit":500})).await["events"]
        .as_array()
        .unwrap()
        .clone()
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["kind"]["kind"].as_str().unwrap().to_owned())
        .collect()
}

fn sha256(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

/// Scenario evidence: steps with expected/observed values and hashes.
struct Evidence {
    scenario: &'static str,
    steps: Vec<Value>,
}

impl Evidence {
    fn new(scenario: &'static str) -> Self {
        Self {
            scenario,
            steps: Vec::new(),
        }
    }

    fn step(&mut self, name: &str, expected: Value, observed: Value, snapshot: Option<&Value>) {
        assert_eq!(observed, expected, "{}: {name}", self.scenario);
        self.steps.push(json!({
            "step": name,
            "expected": expected,
            "observed": observed,
            "snapshotSha256": snapshot.map(sha256),
            "passed": true,
        }));
    }

    fn write(self) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/store");
        std::fs::create_dir_all(&dir).unwrap();
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
        let scenario = json!({
            "scenario": self.scenario,
            "commit": commit,
            "recordedAtMs": chrono::Utc::now().timestamp_millis(),
            "steps": self.steps,
        });
        let body = serde_json::to_vec_pretty(&scenario).unwrap();
        std::fs::write(dir.join(format!("{}.json", self.scenario)), &body).unwrap();
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Value = std::fs::read(&manifest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({"suite":"board_store_e2e","scenarios":{}}));
        manifest["suite"] = json!("board_store_e2e");
        manifest["contract"] = json!("docs/design/project-board/pb0-contract.md");
        manifest["scenarios"][self.scenario] = json!({
            "file": format!("{}.json", self.scenario),
            "sha256": format!("{:x}", Sha256::digest(&body)),
            "steps": scenario["steps"].as_array().unwrap().len(),
            "commit": commit,
            "passed": true,
        });
        std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }
}

/// Cases 1, 2, 5, 6, 10: save-only creation, idempotency, keys, scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_is_save_only_idempotent_and_project_scoped() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut evidence = Evidence::new("create-save-only-idempotent-scoped");
    let luna = space(&c, &core, &f, "project-luna", "luna").await;
    space(&c, &core, &f, "project-orbit", "orbit").await;
    space(&c, &core, &f, "project-lunar", "lunar").await;
    chat(
        &c,
        "project-luna",
        "a1111111-1111-4111-8111-111111111111",
        &luna,
    )
    .await;
    let chats_before = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count = chats(&c).await.len();
            if count == 1 {
                break count;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the source chat row never appeared");

    // Case 10: All-projects creation never guesses a project.
    let ambiguous = refused(
        &c,
        "BoardCreate",
        json!({"requestId":"no-project","title":"Where?"}),
    )
    .await;
    evidence.step(
        "create without project is refused",
        json!("ambiguousProject"),
        json!(ambiguous.code()),
        None,
    );
    let unknown = refused(
        &c,
        "BoardCreate",
        create_request("bad-project", "missing", "X"),
    )
    .await;
    evidence.step(
        "unknown project is refused",
        json!("projectNotFound"),
        json!(unknown.code()),
        None,
    );
    let multi_line = refused(
        &c,
        "BoardCreate",
        create_request("bad-title", "project-luna", "two\nlines"),
    )
    .await;
    evidence.step(
        "multi-line title is refused",
        json!("invalidInput"),
        json!(multi_line.code()),
        None,
    );

    // Case 2: sequential and concurrent retries of one key.
    let first = create(
        &c,
        "create-search",
        "project-luna",
        "Search omits archived projects",
    )
    .await;
    let replay = create(
        &c,
        "create-search",
        "project-luna",
        "Search omits archived projects",
    )
    .await;
    evidence.step(
        "retry returns the identical receipt",
        json!(true),
        json!(first == replay),
        Some(&first),
    );
    let concurrent = create_request("create-concurrent", "project-luna", "Concurrent window");
    let (a, b) = tokio::join!(
        call(&c, "BoardCreate", concurrent.clone()),
        call(&c, "BoardCreate", concurrent.clone())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    evidence.step(
        "two windows with one key get one ticket",
        json!(a["workItemId"]),
        json!(b["workItemId"]),
        Some(&a),
    );
    let conflict = refused(
        &c,
        "BoardCreate",
        create_request("create-search", "project-luna", "A different title"),
    )
    .await;
    evidence.step(
        "same key with different content conflicts",
        json!("idempotencyConflict"),
        json!(conflict.code()),
        None,
    );
    let created_events = events(&c, first["workItemId"].as_str().unwrap()).await;
    evidence.step(
        "exactly one Created event",
        json!(["created"]),
        json!(kinds(&created_events)),
        Some(&json!(created_events)),
    );

    // Case 5: keys per project, never title-derived, unique prefixes.
    let same_title = create(
        &c,
        "create-dup-title",
        "project-luna",
        "Search omits archived projects",
    )
    .await;
    let orbit = create(&c, "create-orbit", "project-orbit", "Orbit telemetry").await;
    let lunar = create(&c, "create-lunar", "project-lunar", "Lunar landing").await;
    evidence.step(
        "keys are per-project monotonic with host-unique prefixes",
        json!(["LUN-1", "LUN-2", "LUN-3", "ORB-1", "LUN2-1"]),
        json!([
            first["key"],
            a["key"],
            same_title["key"],
            orbit["key"],
            lunar["key"]
        ]),
        None,
    );

    // Case 1: zero chats, zero launches, Source link only.
    let from_chat = ok(&c, "BoardCreate", json!({"requestId":"from-chat","projectId":"project-luna","title":"Create a ticket based on this","description":"Later","origin":{"kind":"chat","taskId":"a1111111-1111-4111-8111-111111111111"}})).await;
    let detail = ok(
        &c,
        "BoardRead",
        json!({"workItemId": from_chat["workItemId"]}),
    )
    .await;
    evidence.step(
        "source chat gets a Source link, no working chat",
        json!([["source", "a1111111-1111-4111-8111-111111111111"], null]),
        json!([
            [detail["links"][0]["role"], detail["links"][0]["taskId"]],
            detail["view"]["workingChat"]
        ]),
        Some(&detail),
    );
    evidence.step(
        "saving created zero chats",
        json!(chats_before),
        json!(chats(&c).await.len()),
        None,
    );
    evidence.step(
        "saving started zero provider runs",
        json!(0),
        json!(f.seen.lock().unwrap().len()),
        None,
    );
    evidence.step(
        "a saved ticket is NotStarted in Backlog with no intent",
        json!(["backlog", "notStarted", null]),
        json!([
            detail["view"]["item"]["column"],
            detail["view"]["phase"],
            detail["intent"]
        ]),
        None,
    );

    // Case 6: All projects aggregates without mixing scope.
    let everything = list(&c, all(), json!({})).await;
    let luna_only = list(&c, project("project-luna"), json!({})).await;
    let mixed = everything["items"].as_array().unwrap().iter().any(|view| {
        let key = view["item"]["key"].as_str().unwrap();
        let project = view["item"]["projectId"].as_str().unwrap();
        let prefix = key.rsplit_once('-').unwrap().0;
        !matches!(
            (project, prefix),
            ("project-luna", "LUN") | ("project-orbit", "ORB") | ("project-lunar", "LUN2")
        )
    });
    evidence.step(
        "every All-projects item keeps its own project and key",
        json!(false),
        json!(mixed),
        Some(&everything),
    );
    evidence.step(
        "project scope lists only that project, newest on top",
        json!(["LUN-4", "LUN-3", "LUN-2", "LUN-1"]),
        json!(keys(&luna_only)),
        Some(&luna_only),
    );
    evidence.step(
        "All projects has per-project summaries",
        json!(3),
        json!(everything["projects"].as_array().unwrap().len()),
        None,
    );
    let search = list(&c, all(), json!({"text":"ORBIT TELE"})).await;
    evidence.step(
        "text filter is case-insensitive",
        json!(["ORB-1"]),
        json!(keys(&search)),
        None,
    );
    let paged = list(&c, project("project-luna"), json!({"limit":2})).await;
    let next = list(
        &c,
        project("project-luna"),
        json!({"limit":2,"cursor":paged["nextCursor"]}),
    )
    .await;
    evidence.step(
        "keyset pages are bounded and contiguous",
        json!([["LUN-4", "LUN-3"], ["LUN-2", "LUN-1"]]),
        json!([keys(&paged), keys(&next)]),
        None,
    );
    evidence.write();
    stop(core, c).await;
}

/// Cases 3, 7, 9, 11: field-level revisions, archive, rank, watch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_archive_rank_and_watch_share_one_canonical_state() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut evidence = Evidence::new("update-archive-rank-watch");
    space(&c, &core, &f, "project-luna", "luna").await;
    let one = create(&c, "t1", "project-luna", "First").await;
    let two = create(&c, "t2", "project-luna", "Second").await;
    let three = create(&c, "t3", "project-luna", "Third").await;
    let id = one["workItemId"].as_str().unwrap().to_owned();
    let mut watch = c
        .subscribe("BoardWatch", json!({"scope": project("project-luna")}))
        .await
        .unwrap();
    let head = tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap();

    // Case 3: field-level merge vs stale conflict.
    let renamed = ok(&c, "BoardUpdate", json!({"requestId":"rename","workItemId":id,"expectedRevision":1,"changes":{"title":"First, renamed"}})).await;
    let frame = tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap();
    evidence.step(
        "watch delivers exactly one canonical event for the update",
        json!([["updated"], renamed["eventSeq"]]),
        json!([
            frame["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["kind"]["kind"].clone())
                .collect::<Vec<_>>(),
            frame["events"][0]["seq"]
        ]),
        Some(&frame),
    );
    evidence.step(
        "rename bumps revision and brief revision",
        json!([2, 2]),
        json!([renamed["revision"], renamed["item"]["briefRevision"]]),
        None,
    );
    let stale = refused(&c, "BoardUpdate", json!({"requestId":"stale-title","workItemId":id,"expectedRevision":1,"changes":{"title":"Lost update"}})).await;
    let BoardError::StaleRevision {
        current, expected, ..
    } = &stale
    else {
        panic!("expected staleRevision, got {stale:?}");
    };
    evidence.step(
        "stale same-field edit is refused with the current state",
        json!([1, 2, "First, renamed"]),
        json!([expected, current.item.revision, current.item.title]),
        None,
    );
    let merged = ok(&c, "BoardUpdate", json!({"requestId":"labels","workItemId":id,"expectedRevision":1,"changes":{"labels":["search","search","ui"]}})).await;
    evidence.step(
        "stale unrelated-field edit merges",
        json!([3, ["search", "ui"], "First, renamed"]),
        json!([
            merged["revision"],
            merged["item"]["labels"],
            merged["item"]["title"]
        ]),
        None,
    );
    let noop = ok(&c, "BoardUpdate", json!({"requestId":"noop","workItemId":id,"expectedRevision":3,"changes":{"labels":["search","ui"]}})).await;
    evidence.step(
        "a no-op edit adds no event and keeps the revision",
        json!([3, null]),
        json!([noop["revision"], noop["eventSeq"]]),
        None,
    );

    // Case 9: reorder has no execution effect.
    let before = list(&c, project("project-luna"), json!({})).await;
    evidence.step(
        "initial order is newest first",
        json!(["LUN-3", "LUN-2", "LUN-1"]),
        json!(keys(&before)),
        Some(&before),
    );
    let rank = json!({"requestId":"rank-1","workItemId":id,"afterId":three["workItemId"]});
    let ranked = ok(&c, "BoardRank", rank.clone()).await;
    let replayed = ok(&c, "BoardRank", rank).await;
    let after = list(&c, project("project-luna"), json!({})).await;
    evidence.step(
        "reorder moves only within the column",
        json!(["LUN-3", "LUN-1", "LUN-2"]),
        json!(keys(&after)),
        Some(&after),
    );
    evidence.step(
        "reorder keeps the revision and replays identically",
        json!([3, true]),
        json!([ranked["revision"], ranked == replayed]),
        None,
    );
    let detail = ok(&c, "BoardRead", json!({"workItemId":id})).await;
    let item_events = events(&c, &id).await;
    let reorder_events: Vec<&Value> = item_events
        .iter()
        .filter(|event| event["kind"]["kind"] == "reordered")
        .collect();
    evidence.step(
        "one non-activity Reordered event, no intent, no runs",
        json!([1, false, null, 0, "backlog"]),
        json!([
            reorder_events.len(),
            reorder_events[0]["activity"],
            detail["intent"],
            f.seen.lock().unwrap().len(),
            detail["view"]["item"]["column"]
        ]),
        Some(&json!(item_events)),
    );
    let bad_neighbor = refused(
        &c,
        "BoardRank",
        json!({"requestId":"rank-bad","workItemId":id,"afterId":"wi-missing"}),
    )
    .await;
    evidence.step(
        "an unknown neighbor is refused",
        json!("notFound"),
        json!(bad_neighbor.code()),
        None,
    );

    // Case 7: archive hides but preserves.
    let archived = ok(
        &c,
        "BoardArchive",
        json!({"requestId":"archive-2","workItemId":two["workItemId"],"archived":true}),
    )
    .await;
    let visible = list(&c, project("project-luna"), json!({})).await;
    let only = list(&c, project("project-luna"), json!({"archived":"only"})).await;
    evidence.step(
        "archived ticket is hidden from the board but listed in Archive",
        json!([["LUN-3", "LUN-1"], ["LUN-2"]]),
        json!([keys(&visible), keys(&only)]),
        None,
    );
    let replacement = create(&c, "t4", "project-luna", "After archive").await;
    evidence.step(
        "archived keys are never reused",
        json!("LUN-4"),
        replacement["key"].clone(),
        None,
    );
    let restored = ok(&c, "BoardArchive", json!({"requestId":"unarchive-2","workItemId":two["workItemId"],"archived":false,"expectedRevision":archived["revision"]})).await;
    let back = list(&c, project("project-luna"), json!({})).await;
    evidence.step(
        "unarchive restores the ticket in place with its history",
        json!([
            ["LUN-4", "LUN-3", "LUN-1", "LUN-2"],
            ["created", "archived", "unarchived"]
        ]),
        json!([
            keys(&back),
            kinds(&events(&c, two["workItemId"].as_str().unwrap()).await)
        ]),
        Some(&back),
    );
    evidence.step(
        "archive changes revision only",
        json!([3, "backlog"]),
        json!([restored["revision"], restored["column"]]),
        None,
    );
    evidence.step(
        "nothing in this scenario started a provider run",
        json!(0),
        json!(f.seen.lock().unwrap().len()),
        None,
    );
    drop(head);
    evidence.write();
    stop(core, c).await;
}

/// Cases 4, 5, 8: restart and backup/restore of every board table.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_and_backup_restore_preserve_tickets_keys_order_and_journal() {
    let f = fixture();
    let mut evidence = Evidence::new("restart-backup-restore");
    let (core, c) = boot(&f);
    space(&c, &core, &f, "project-luna", "luna").await;
    let first = create(&c, "r1", "project-luna", "Restart one").await;
    let second = create(&c, "r2", "project-luna", "Restart two").await;
    ok(&c, "BoardRank", json!({"requestId":"r-rank","workItemId":second["workItemId"],"afterId":first["workItemId"]})).await;
    ok(&c, "BoardUpdate", json!({"requestId":"r-edit","workItemId":first["workItemId"],"expectedRevision":1,"changes":{"description":"Edited before restart"}})).await;
    let before = list(&c, project("project-luna"), json!({})).await;
    evidence.step(
        "a reorder before restart is part of the saved order",
        json!(["LUN-1", "LUN-2"]),
        json!(keys(&before)),
        Some(&before),
    );
    let journal_before = ok(&c, "BoardEvents", json!({"limit":500})).await;
    stop(core, c).await;

    let (core, c) = boot(&f);
    let after = list(&c, project("project-luna"), json!({})).await;
    let journal_after = ok(&c, "BoardEvents", json!({"limit":500})).await;
    evidence.step(
        "restart preserves tickets, order and views",
        json!(before["items"]),
        after["items"].clone(),
        Some(&after),
    );
    evidence.step(
        "restart preserves the journal",
        json!(sha256(&journal_before["events"])),
        json!(sha256(&journal_after["events"])),
        Some(&journal_after),
    );
    let replay = create(&c, "r1", "project-luna", "Restart one").await;
    evidence.step(
        "a pre-restart key replays its receipt",
        first["workItemId"].clone(),
        replay["workItemId"].clone(),
        None,
    );
    let third = create(&c, "r3", "project-luna", "After restart").await;
    evidence.step(
        "numbering continues after restart",
        json!("LUN-3"),
        third["key"].clone(),
        None,
    );
    let backed_up = list(&c, project("project-luna"), json!({})).await;
    let journal_backed_up = ok(&c, "BoardEvents", json!({"limit":500})).await;
    stop(core, c).await;

    let service = BackupService::new(
        IsolatedRoot::open(&f.root).unwrap(),
        RecoveryKey::injected([19; 32]),
    );
    let backup = service.export(&f.backups.join("board.lunabk")).unwrap();
    evidence.step(
        "archive records schema 19",
        json!(19),
        json!(backup.manifest.databases[0].schema_version),
        None,
    );

    let (core, c) = boot(&f);
    let lost = create(&c, "r4", "project-luna", "Created after the backup").await;
    evidence.step(
        "post-backup ticket exists before restore",
        json!("LUN-4"),
        lost["key"].clone(),
        None,
    );
    stop(core, c).await;

    // Restore onto a reinstalled data root at the original path (absolute
    // project/attachment paths require it). The backup service refuses to
    // stage over a root holding files outside its allowlist, which every root
    // a live engine has used does, so the old root is moved aside first.
    let previous_root = f.root.parent().unwrap().join("previous-luna");
    std::fs::rename(&f.root, &previous_root).unwrap();
    std::fs::create_dir(&f.root).unwrap();
    std::fs::write(
        f.root.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
    std::fs::copy(previous_root.join("device-id"), f.root.join("device-id")).unwrap();
    let target = BackupService::new(
        IsolatedRoot::open(&f.root).unwrap(),
        RecoveryKey::injected([19; 32]),
    );
    let staged = target.stage_restore(&backup.archive).unwrap();
    target.verify_staged_for_tests(&staged).unwrap();
    target.activate(staged).unwrap();

    let (core, c) = boot(&f);
    let restored = list(&c, project("project-luna"), json!({})).await;
    let journal_restored = ok(&c, "BoardEvents", json!({"limit":500})).await;
    evidence.step(
        "restore round-trips tickets and order exactly",
        json!(backed_up["items"]),
        restored["items"].clone(),
        Some(&restored),
    );
    evidence.step(
        "restore round-trips the journal",
        json!(sha256(&journal_backed_up["events"])),
        json!(sha256(&journal_restored["events"])),
        Some(&journal_restored),
    );
    let receipt = ok(&c, "BoardGetReceipt", json!({"requestId":"r3"})).await;
    evidence.step(
        "restore round-trips idempotency receipts",
        third["workItemId"].clone(),
        receipt["workItemId"].clone(),
        None,
    );
    let gone = refused(&c, "BoardRead", json!({"workItemId": lost["workItemId"]})).await;
    evidence.step(
        "the post-backup ticket is gone after restore",
        json!("notFound"),
        json!(gone.code()),
        None,
    );
    evidence.step(
        "no step started a provider run",
        json!(0),
        json!(f.seen.lock().unwrap().len()),
        None,
    );
    evidence.write();
    stop(core, c).await;
}

/// Cases 12, 13: every contract method is routed; older profiles upgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_board_method_is_routed_and_older_profiles_upgrade_additively() {
    let f = fixture();
    let mut evidence = Evidence::new("routing-and-upgrade");
    // A profile from before the board: schema 18 with existing data.
    let profile = f.root.join("profiles/local");
    let store = luna_sync::DocsStore::open(&profile).unwrap();
    store
        .save_snapshot("legacy-doc", b"pre-board data")
        .unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(profile.join("docs.sqlite3")).unwrap();
    conn.execute_batch(
        "DROP TABLE board_projects; DROP TABLE board_items; DROP TABLE board_mutations;
         DROP TABLE board_events; DROP TABLE board_task_links; DROP TABLE board_issue_refs;
         DROP TABLE board_launches; DROP TABLE board_outcomes; DROP TABLE board_intents;
         DROP TABLE board_review_runs; DROP TABLE board_authoring_sessions;
         DROP TABLE board_authoring_entries; DROP TABLE board_dependencies;
         DELETE FROM schema_migrations WHERE version > 18;",
    )
    .unwrap();
    drop(conn);

    let (core, c) = boot(&f);
    space(&c, &core, &f, "project-luna", "luna").await;
    let created = create(&c, "upgrade", "project-luna", "After upgrade").await;
    evidence.step(
        "the upgraded profile accepts tickets",
        json!("LUN-1"),
        created["key"].clone(),
        None,
    );
    for method in luna_rpc::methods::BOARD_METHODS {
        if matches!(*method, "BoardWatch" | "BoardAuthoringWatch") {
            continue;
        }
        match call(&c, method, json!({})).await {
            Err(RpcError::UnknownMethod(name)) => panic!("{name} is not routed"),
            Err(RpcError::Failed(message)) => assert!(
                BoardError::from_rpc_message(&message).is_some(),
                "{method} failed without a structured board error: {message}"
            ),
            Err(RpcError::BadParams(_)) | Ok(_) => {}
            Err(other) => panic!("{method}: {other}"),
        }
    }
    evidence.step(
        "every contract method is routed with structured errors",
        json!(luna_rpc::methods::BOARD_METHODS.len()),
        json!(luna_rpc::methods::BOARD_METHODS.len()),
        None,
    );
    let other_host = refused(
        &c,
        "BoardList",
        json!({"targetDeviceId":"another-host","scope":all()}),
    )
    .await;
    evidence.step(
        "board methods are owner-local",
        json!("unavailable"),
        json!(other_host.code()),
        None,
    );
    stop(core, c).await;
    let store = luna_sync::DocsStore::open(&profile).unwrap();
    evidence.step(
        "pre-board data survives the additive migration",
        json!(b"pre-board data".to_vec()),
        json!(store.load_snapshot("legacy-doc").unwrap().unwrap()),
        None,
    );
    drop(store);
    let conn = rusqlite::Connection::open(profile.join("docs.sqlite3")).unwrap();
    let version: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    evidence.step("schema is 19", json!(19), json!(version), None);
    evidence.write();
}

/// Case 14: the heaviest board operations answer over real RPC on an
/// explicit 2 MiB worker stack (an overflow aborts the whole test binary).
#[test]
fn heavy_board_rpcs_fit_a_two_mib_worker_stack() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(2 * 1024 * 1024)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let f = fixture();
        let (core, c) = boot(&f);
        let mut evidence = Evidence::new("two-mib-worker-stack");
        let path = space(&c, &core, &f, "project-luna", "luna").await;
        let chat_id = "a2222222-2222-4222-8222-222222222222";
        chat(&c, "project-luna", chat_id, &path).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while chats(&c).await.is_empty() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the chat row never appeared");
        let moved = create(&c, "stack-move", "project-luna", "Moved to Ready").await;
        let started = create(&c, "stack-start", "project-luna", "Started explicitly").await;
        let calls = [
            (
                "BoardMove",
                json!({"requestId":"stack-move-1","workItemId":moved["workItemId"],"to":"ready"}),
            ),
            (
                "BoardStart",
                json!({"requestId":"stack-start-1","workItemId":started["workItemId"]}),
            ),
            (
                "BoardConvertChat",
                json!({"requestId":"stack-convert-1","taskId":chat_id,"title":"Convert me","description":"Converted in place"}),
            ),
            (
                "BoardReviewStart",
                json!({"requestId":"stack-review-1","workItemId":started["workItemId"]}),
            ),
        ];
        for (method, params) in calls {
            let outcome = match call(&c, method, params).await {
                Ok(_) => "ok".to_owned(),
                Err(RpcError::Failed(message)) => BoardError::from_rpc_message(&message)
                    .map(|error| error.code())
                    .unwrap_or_else(|| panic!("{method}: unstructured failure {message}")),
                Err(RpcError::BadParams(message)) => panic!("{method}: bad params {message}"),
                Err(other) => panic!("{method}: the RPC did not answer: {other}"),
            };
            evidence.step(
                &format!("{method} answers on a 2 MiB worker ({outcome})"),
                json!(true),
                json!(!outcome.is_empty()),
                None,
            );
        }
        evidence.write();
        stop(core, c).await;
    });
}
