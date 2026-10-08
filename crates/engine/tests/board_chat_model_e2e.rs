//! PB-1 chat-model E2E (slice A3): real engine over the in-memory RPC
//! transport → `Board*` / legacy creation RPCs and the agent tool backend →
//! SQLite, workspace registry and task identity → deterministic protocol
//! peer. Contract: docs/design/project-board/pb0-contract.md §6, §7.5–§7.6,
//! §11.5; product: docs/pending-tasks/devin-project-board/chat-model.md §7–§8.
//!
//! Failure cases, written before the chat-model implementation:
//!
//! 1. Saving a ticket (Board or chat origin) creates a chat, a Working link,
//!    a launch or a provider run.
//! 2. Creating a ticket's first worker as a new main chat (the creation
//!    half of Board Start) creates more than one chat, hides it from the
//!    sidebar (side parent, fork source or child metadata), forgets the
//!    frozen `MainChat` container, or sends anything while only preparing.
//! 3. A main parent's fresh or with-history side chat lands in the sidebar,
//!    nests under the wrong chat, duplicates on retry, misreports its
//!    context origin, or sends its first message twice.
//! 4. Converting a main or a side chat changes its id, history, provider
//!    binding, settings, placement, draft or active run; resends a prompt;
//!    creates a second link on a duplicate request; or accepts a temporary
//!    authoring session, a reviewer or an already-working chat.
//! 5. Three tickets started from one main chat yield anything but three
//!    distinct side chats under that main, or give the coordinator a
//!    working badge.
//! 6. Go to chat after closing tabs or a restart depends on tabs, layout or
//!    titles, relaunches/resends, or loses the exact side chat.
//! 7. Renaming the ticket or its working chat leaves two titles, renames the
//!    parent/source discussion or a historical attempt, or provider
//!    auto-titling drifts the working chat away from its ticket.
//! 8. A source/reference discussion shows the working ticket badge, or the
//!    worker loses it.
//! 9. Legacy nested child records lose ids, history, bindings, permissions,
//!    delivery receipts or placements in migration; an active descendant is
//!    re-owned mid-run; nesting continues after migration.
//! 10. An archived/unavailable parent makes the worker unreachable, or
//!     recovery rewrites the parent, restarts or cancels the worker.
//! 11. A side chat (also one shown as its own workbench or as the selected
//!     tab) creates a nested child, a native/message fork, a context
//!     snapshot or a portable context copy through a UI-equivalent RPC
//!     (`CreateSideChat`, `CreateChildTab`, `forkTask`, `createChat` with
//!     forged lineage, `PreviewContextTransfer`, `BoardCreateSideChat`) or a
//!     tool path (`create_thread` preflight, `manage_workspace` forkThread,
//!     including forking the main as a parent proxy); a rejection lacks the
//!     allowed alternatives.
//! 12. Side → new main handoff omits the brief, inherits the transcript,
//!     hides the new chat from the sidebar, grants it child/lifecycle
//!     ownership, moves or changes the caller, upgrades access, or sends
//!     without (or twice with) an explicit start.
//! 13. Side → sibling becomes a child of the side caller, loses the side
//!     creator provenance, takes the main's model instead of the creator's,
//!     exceeds the creator's access ceiling, routes results to the main by
//!     default, or accepts a caller-supplied parent.
//! 14. A side chat makes itself a root or attaches itself under a new or
//!     different parent, including create-root-then-adopt sequences (reuse
//!     of its id, forged lineage, Attempt links, conversion).
//! 15. Canonical identity (role, main parent, container, permitted actions)
//!     changes with the selected tab, an own workbench, a restart or a
//!     title.
//! 16. A new main chat cannot own fresh/with-context side chats, or the
//!     original parent gains control over it.
//! 17. Reparenting a ticket worker, idle or active, through a second-parent
//!     create, an Attempt link from another container or a project move
//!     succeeds, duplicates the worker or resends its first message.
//! 18. A rejection is not a decodable structured error naming the allowed
//!     alternative or the canonical location.
//!
//! Every scenario writes a JSON manifest (steps, expected/observed, SHA-256
//! of before/after snapshots, commit) under `target/qa/board/chat-model/`.
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::cross_task_tools::{CrossTaskAuthority, CrossTaskBackend};
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

const SUITE: &str = "board_chat_model_e2e";

/// One provider run as the deterministic peer saw it.
#[derive(Debug, Clone)]
struct Run {
    prompt: String,
    model: Option<String>,
}

/// Records every run. A prompt containing `HOLD` keeps its turn active until
/// it is interrupted; everything else completes immediately.
struct Peer(Arc<Mutex<Vec<Run>>>);

#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Board chat-model protocol peer"
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
        Ok(["fixture", "fixture-b"]
            .into_iter()
            .map(|id| Model {
                id: id.into(),
                label: id.into(),
                description: None,
                reasoning_levels: Vec::new(),
                options: Vec::new(),
                first_seen_at_ms: None,
            })
            .collect())
    }
    async fn run(
        &self,
        request: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let hold = request.prompt.contains("HOLD");
        self.0.lock().unwrap().push(Run {
            prompt: request.prompt.clone(),
            model: request.model.clone(),
        });
        if hold {
            return Ok(futures::stream::iter(vec![Ok(AgentEvent::TextDelta {
                text: "HELD_TURN_STARTED".into(),
            })])
            .chain(futures::stream::pending())
            .boxed());
        }
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::TextDelta {
                text: "PEER_ACK".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            }),
        ])
        .boxed())
    }
}

struct Fixture {
    _parent: tempfile::TempDir,
    root: PathBuf,
    projects: PathBuf,
    runs: Arc<Mutex<Vec<Run>>>,
}

fn fixture() -> Fixture {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("synthetic-luna");
    let projects = parent.path().join("projects");
    for dir in [&root, &projects] {
        std::fs::create_dir_all(dir).unwrap();
    }
    Fixture {
        _parent: parent,
        root,
        projects,
        runs: Arc::new(Mutex::new(Vec::new())),
    }
}

fn boot(f: &Fixture) -> (EngineCore, RpcClient) {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer(f.runs.clone())));
    let accounts_root = f.root.parent().unwrap().join("provider-fixtures");
    let accounts = AgentAccountsConfig {
        data_dir: accounts_root.join("accounts"),
        claude_config_dir: accounts_root.join("claude"),
        claude_config_file: accounts_root.join("claude.json"),
        codex_home: accounts_root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(&f.root).unwrap(),
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

/// The error code plus whether alternatives were offered (case 18).
fn refusal(error: &BoardError) -> Value {
    let allowed = match error {
        BoardError::InvalidRole { allowed, .. } => allowed.len(),
        _ => 0,
    };
    json!({"code": error.code(), "alternatives": allowed > 0})
}

fn config(model: &str, mode: &str) -> Value {
    json!({"harness":"mock","model":model,"sandbox":"workspace-write",
        "permissionMode":mode,"effectivePermissionMode":mode})
}

async fn project(c: &RpcClient, core: &EngineCore, f: &Fixture, id: &str) -> String {
    let path = f.projects.join(id);
    std::fs::create_dir_all(&path).unwrap();
    let path = path.canonicalize().unwrap().to_string_lossy().to_string();
    ok(
        c,
        "Mutate",
        json!({"op":"createSpace","spaceId":id,"deviceId":core.device_id,"path":path}),
    )
    .await;
    path
}

/// An ordinary main chat (Cmd+N) with a provider configuration.
async fn main_chat(c: &RpcClient, project: &str, cwd: &str, title: &str, cfg: Value) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let receipt = ok(c, "ExecuteTaskCommand", json!({"commandId":format!("create-{id}"),"taskId":id,
        "expectedTaskRevision":0,"intent":{"kind":"createChat","spaceId":project,"cwd":cwd,"config":cfg}})).await;
    assert_eq!(receipt["status"], "applied", "{receipt}");
    let renamed = command(
        c,
        &id,
        &format!("title-{id}"),
        json!({"kind":"rename","title":title}),
    )
    .await;
    assert_eq!(renamed["status"], "applied", "{renamed}");
    id
}

async fn snapshot(c: &RpcClient, task: &str) -> Value {
    ok(c, "GetTaskCommandSnapshot", json!({"taskId":task})).await["snapshot"].clone()
}

/// A task command with the fences the engine expects.
async fn command(c: &RpcClient, task: &str, id: &str, intent: Value) -> Value {
    let snapshot = snapshot(c, task).await;
    let mut value = json!({"commandId":id,"actorId":"board-chat-e2e","clientId":"board-chat-e2e",
        "clientSessionId":"e2e","taskId":task,"intent":intent,
        "expectedTaskRevision":snapshot["taskRevision"]});
    if let Some(generation) = snapshot["bindingGeneration"].as_u64() {
        value["expectedBindingGeneration"] = generation.into();
    }
    if let Some(turn) = snapshot["activeTurnId"].as_str() {
        value["expectedTurnId"] = turn.into();
    }
    ok(c, "ExecuteTaskCommand", value).await
}

async fn send(c: &RpcClient, task: &str, text: &str) -> Value {
    let id = format!("send-{}", uuid::Uuid::new_v4());
    let receipt = command(
        c,
        task,
        &id,
        json!({"kind":"queueMessage","messageId":id,"text":text,"holdForTurnEnd":true}),
    )
    .await;
    assert_eq!(receipt["status"], "applied", "{receipt}");
    receipt
}

async fn interrupt(c: &RpcClient, task: &str) {
    let receipt = command(
        c,
        task,
        &format!("stop-{}", uuid::Uuid::new_v4()),
        json!({"kind":"session","command":{"kind":"interrupt"},"transfers":[]}),
    )
    .await;
    assert_eq!(receipt["status"], "applied", "{receipt}");
}

async fn chats(c: &RpcClient) -> Vec<Value> {
    let mut watch = c.subscribe("WatchChats", json!({})).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap();
    first.as_array().cloned().unwrap_or_default()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("chat {id} is not published"))
}

/// True when the chat is an independent sidebar root (no parent-origin
/// metadata of any kind).
fn sidebar_root(row: &Value) -> bool {
    row.get("sideChatParentId").is_none()
        && row.get("forkSourceTaskId").is_none()
        && row.get("childThread").is_none()
}

async fn identity(c: &RpcClient, task: &str) -> Value {
    ok(c, "BoardChatIdentity", json!({"taskId":task})).await
}

/// Where a chat lives (role, parent, container) — what no request may move.
fn container_identity(identity: &Value) -> Value {
    json!({
        "role": identity["role"],
        "mainParentId": identity.get("mainParentId"),
        "displayContainerId": identity["displayContainerId"],
    })
}

async fn ticket(
    c: &RpcClient,
    key: &str,
    project: &str,
    title: &str,
    origin: Option<&str>,
) -> Value {
    let mut request = json!({"requestId":key,"projectId":project,"title":title,
        "description":format!("Brief for {title}")});
    if let Some(origin) = origin {
        request["origin"] = json!({"kind":"chat","taskId":origin});
    }
    ok(c, "BoardCreate", request).await
}

fn binding(receipt: &Value) -> Value {
    json!({"workItemId":receipt["workItemId"],"expectedRevision":receipt["revision"]})
}

async fn read(c: &RpcClient, item: &str) -> Value {
    ok(c, "BoardRead", json!({"workItemId":item})).await
}

fn working_links(detail: &Value) -> Vec<Value> {
    detail["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|link| link["role"] == "working" && link["active"] == true)
        .cloned()
        .collect()
}

/// The frozen parent snapshot a side-chat service row carries (read-only).
fn side_snapshot(f: &Fixture, id: &str) -> Option<String> {
    let path = f.root.join("profiles").join("local").join("docs.sqlite3");
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    db.query_row("SELECT context FROM side_chats WHERE id=?1", [id], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

fn runs(f: &Fixture) -> Vec<Run> {
    f.runs.lock().unwrap().clone()
}

fn runs_with(f: &Fixture, needle: &str) -> usize {
    runs(f)
        .iter()
        .filter(|run| run.prompt.contains(needle))
        .count()
}

async fn wait_for(what: &str, seconds: u64, check: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !check() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_settled(core: &EngineCore, task: &str) {
    wait_for(&format!("{task} settles"), 10, || {
        !core.sessions.turn_in_flight(task)
    })
    .await;
}

fn authority(core: &EngineCore, caller: &str, project: &str) -> CrossTaskAuthority {
    CrossTaskAuthority {
        caller_task_id: caller.into(),
        local_environment_id: core.device_id.clone(),
        visible_project_ids: std::collections::HashSet::from([project.to_owned()]),
        allow_projectless: false,
        granted_scopes: std::collections::HashSet::from([
            "threads.read".to_owned(),
            "board.read".to_owned(),
            "board.write".to_owned(),
        ]),
        explicit_user_request: false,
        delivery: luna_proto::RuntimeDelivery::CodexMcpTools,
    }
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

    /// Before/after snapshots that must be identical.
    fn unchanged(&mut self, name: &str, before: &Value, after: &Value) {
        assert_eq!(after, before, "{}: {name}", self.scenario);
        self.steps.push(json!({
            "step": name,
            "expected": "unchanged",
            "observed": "unchanged",
            "beforeSha256": sha256(before),
            "afterSha256": sha256(after),
            "passed": true,
        }));
    }

    fn write(self) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/chat-model");
        std::fs::create_dir_all(&dir).unwrap();
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
        let scenario = json!({
            "suite": SUITE,
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
            .unwrap_or_else(|| json!({"suite":SUITE,"scenarios":{}}));
        manifest["suite"] = json!(SUITE);
        manifest["contract"] = json!("docs/design/project-board/pb0-contract.md");
        manifest["spec"] = json!("docs/pending-tasks/devin-project-board/chat-model.md");
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

/// Cases 1, 2, 8, 17 (standalone main ticket): save creates nothing; the
/// creation half of Board Start makes exactly one sidebar main chat with a
/// frozen `MainChat` container; a second requester reuses it in place.
#[tokio::test(flavor = "multi_thread")]
async fn save_creates_nothing_and_board_start_makes_one_main_worker() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut ev = Evidence::new("save-and-main-worker");
    let path = project(&c, &core, &f, "chat-model").await;
    let source = main_chat(
        &c,
        "chat-model",
        &path,
        "Source discussion",
        config("fixture", "ask"),
    )
    .await;
    let other_main = main_chat(
        &c,
        "chat-model",
        &path,
        "Another main",
        config("fixture", "ask"),
    )
    .await;
    let before_chats = chats(&c).await;

    // Save from Board and from a normal chat: zero chats, zero runs.
    let board_saved = ticket(&c, "save-board", "chat-model", "Saved on Board", None).await;
    let chat_saved = ticket(
        &c,
        "save-chat",
        "chat-model",
        "Fix the importer",
        Some(&source),
    )
    .await;
    let after_save = chats(&c).await;
    ev.step(
        "save creates zero chats",
        json!({"chats": before_chats.len(), "runs": 0}),
        json!({"chats": after_save.len(), "runs": runs(&f).len()}),
        Some(&json!(after_save)),
    );
    let detail = read(&c, chat_saved["workItemId"].as_str().unwrap()).await;
    ev.step(
        "chat-origin save links only a Source",
        json!({"links": [["source", source]], "working": null}),
        json!({
            "links": detail["links"].as_array().unwrap().iter()
                .map(|link| json!([link["role"], link["taskId"]])).collect::<Vec<_>>(),
            "working": detail["view"].get("workingChat"),
        }),
        Some(&detail),
    );
    let badges = ok(&c, "BoardChatBadges", json!({"taskIds":[source]})).await;
    ev.step(
        "source discussion: related receipt, no working badge",
        json!({"badges": 0, "related": [["source", chat_saved["key"]]]}),
        json!({
            "badges": badges["badges"].as_array().unwrap().len(),
            "related": badges["related"].as_array().unwrap().iter()
                .map(|badge| json!([badge["role"], badge["key"]])).collect::<Vec<_>>(),
        }),
        Some(&badges),
    );

    // Board Start's creation half: one new main chat, prepared, linked.
    let start = json!({"requestId":"start-main","projectId":"chat-model","ticket":binding(&board_saved),
        "execution":{"harness":"mock","model":"fixture"}});
    let created = ok(&c, "BoardCreateMainChat", start.clone()).await;
    let worker = created["taskId"].as_str().unwrap().to_owned();
    let rows = chats(&c).await;
    ev.step(
        "one main sidebar chat, nothing sent",
        json!({"role":"main","mainParentId":null,"launch":"created","reused":null,
            "chats": after_save.len() + 1, "sidebarRoot": true, "title":"Saved on Board",
            "navigation":{"kind":"mainChat","taskId":worker}, "runs": 0}),
        json!({"role":created["role"],"mainParentId":created.get("mainParentId"),
            "launch":created["launch"],"reused":created.get("reused"),"chats":rows.len(),
            "sidebarRoot": sidebar_root(row(&rows, &worker)),
            "title": row(&rows, &worker)["title"],
            "navigation":created["navigation"],"runs":runs(&f).len()}),
        Some(&created),
    );
    let worker_identity = identity(&c, &worker).await;
    ev.step(
        "frozen MainChat container, parent locked, no reparent capability",
        json!({"purpose":"ticketWorker","container":{"kind":"mainChat"},"locked":true,
            "reparent":false,"display":worker}),
        json!({"purpose":worker_identity["purpose"]["kind"],
            "container":worker_identity["originalContainer"],
            "locked":worker_identity["parentLocked"],
            "reparent":worker_identity["permitted"]["reparent"],
            "display":worker_identity["displayContainerId"]}),
        Some(&worker_identity),
    );
    let replay = ok(&c, "BoardCreateMainChat", start).await;
    ev.step(
        "retry returns the same worker",
        json!({"taskId":worker,"chats":rows.len()}),
        json!({"taskId":replay["taskId"],"chats":chats(&c).await.len()}),
        None,
    );
    let badges = ok(&c, "BoardChatBadges", json!({"taskIds":[worker]})).await;
    ev.step(
        "worker shows the working ticket badge",
        json!([["working", board_saved["key"]]]),
        json!(
            badges["badges"]
                .as_array()
                .unwrap()
                .iter()
                .map(|badge| json!([badge["role"], badge["key"]]))
                .collect::<Vec<_>>()
        ),
        Some(&badges),
    );

    // A second main chat asking for the same ticket reuses the worker.
    let detail_before = read(&c, board_saved["workItemId"].as_str().unwrap()).await;
    let reused = ok(&c, "BoardCreateSideChat", json!({"requestId":"second-parent","callerTaskId":other_main,
        "ticket":{"workItemId":board_saved["workItemId"],"expectedRevision":detail_before["view"]["item"]["revision"]}})).await;
    let detail_after = read(&c, board_saved["workItemId"].as_str().unwrap()).await;
    ev.step(
        "second parent reuses the standalone main worker",
        json!({"taskId":worker,"reused":true,"role":"main","navigation":{"kind":"mainChat","taskId":worker},
            "chats": rows.len(), "runs": 0, "workingLinks": 1}),
        json!({"taskId":reused["taskId"],"reused":reused["reused"],"role":reused["role"],
            "navigation":reused["navigation"],"chats":chats(&c).await.len(),"runs":runs(&f).len(),
            "workingLinks": working_links(&detail_after).len()}),
        Some(&reused),
    );
    ev.unchanged(
        "worker identity and links unchanged by the second request",
        &json!([worker_identity, detail_before["links"]]),
        &json!([identity(&c, &worker).await, detail_after["links"]]),
    );
    ev.write();
    stop(core, c).await;
}

/// Cases 3 and 5: a main parent's fresh and with-history side chats, each
/// sent once; three tickets become three distinct side chats under the
/// coordinating main, which keeps no working badge.
#[tokio::test(flavor = "multi_thread")]
async fn main_parent_side_chats_and_three_tickets() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut ev = Evidence::new("main-parent-side-chats");
    let path = project(&c, &core, &f, "chat-model").await;
    let main = main_chat(
        &c,
        "chat-model",
        &path,
        "Coordinator",
        config("fixture", "ask"),
    )
    .await;
    send(&c, &main, "MAIN_HISTORY_SENTINEL").await;
    wait_for("main history run", 10, || {
        runs_with(&f, "MAIN_HISTORY_SENTINEL") == 1
    })
    .await;
    wait_settled(&core, &main).await;

    // Fresh: a draft child of the main, started once with its brief.
    let fresh = json!({"requestId":"fresh-side","callerTaskId":main,"context":{"kind":"fresh"},
        "title":"Fresh helper","prompt":"FRESH_SIDE_BRIEF","start":true});
    let created = ok(&c, "BoardCreateSideChat", fresh.clone()).await;
    let fresh_id = created["taskId"].as_str().unwrap().to_owned();
    wait_for("fresh side run", 10, || {
        runs_with(&f, "FRESH_SIDE_BRIEF") == 1
    })
    .await;
    wait_settled(&core, &fresh_id).await;
    let replay = ok(&c, "BoardCreateSideChat", fresh).await;
    let rows = chats(&c).await;
    let fresh_run = runs(&f)
        .into_iter()
        .find(|run| run.prompt.contains("FRESH_SIDE_BRIEF"))
        .unwrap();
    ev.step(
        "fresh side chat under the main, sent once, no sidebar root",
        json!({"role":"side","mainParentId":main,"contextOrigin":"fresh","launch":"queued",
            "creator":{"taskId":main,"relation":"createdBy"},"childParent":main,"sidebarRoot":false,
            "replayTaskId":fresh_id,"sends":1,"inheritedHistory":false,
            "navigation":{"kind":"sideChat","mainTaskId":main,"sideTaskId":fresh_id}}),
        json!({"role":created["role"],"mainParentId":created["mainParentId"],
            "contextOrigin":created["contextOrigin"],"launch":created["launch"],
            "creator":created["creator"],"childParent":row(&rows, &fresh_id).pointer("/childThread/parentTaskId"),
            "sidebarRoot":sidebar_root(row(&rows, &fresh_id)),"replayTaskId":replay["taskId"],
            "sends":runs_with(&f, "FRESH_SIDE_BRIEF"),
            "inheritedHistory":fresh_run.prompt.contains("MAIN_HISTORY_SENTINEL")
                || side_snapshot(&f, &fresh_id).is_some(),
            "navigation":created["navigation"]}),
        Some(&created),
    );

    // With history: the side-chat service (visible snapshot for this
    // provider), still owned by the main, also sent once.
    let context = json!({"requestId":"context-side","callerTaskId":main,"context":{"kind":"withContext"},
        "prompt":"CONTEXT_SIDE_BRIEF","start":true});
    let created = ok(&c, "BoardCreateSideChat", context.clone()).await;
    let context_id = created["taskId"].as_str().unwrap().to_owned();
    wait_for("context side run", 10, || {
        runs_with(&f, "CONTEXT_SIDE_BRIEF") == 1
    })
    .await;
    wait_settled(&core, &context_id).await;
    let replay = ok(&c, "BoardCreateSideChat", context).await;
    let rows = chats(&c).await;
    let context_run = runs(&f)
        .into_iter()
        .find(|run| run.prompt.contains("CONTEXT_SIDE_BRIEF"))
        .unwrap();
    ev.step(
        "with-history side chat reports its snapshot origin",
        json!({"role":"side","mainParentId":main,"contextOrigin":"contextSnapshot",
            "source":{"taskId":main,"relation":"forkedFrom"},"sideChatParentId":main,
            "replayTaskId":context_id,"sends":1,"inheritedHistory":true}),
        json!({"role":created["role"],"mainParentId":created["mainParentId"],
            "contextOrigin":created["contextOrigin"],
            "source":identity(&c, &context_id).await["source"],
            "sideChatParentId":row(&rows, &context_id)["sideChatParentId"],
            "replayTaskId":replay["taskId"],"sends":runs_with(&f, "CONTEXT_SIDE_BRIEF"),
            "inheritedHistory":side_snapshot(&f, &context_id)
                .is_some_and(|snapshot| snapshot.contains("MAIN_HISTORY_SENTINEL"))
                && !context_run.prompt.is_empty()}),
        Some(&created),
    );
    let refused_key = refused(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"context-side","callerTaskId":main,
        "context":{"kind":"fresh"}}),
    )
    .await;
    ev.step(
        "a request id reused with other content is a conflict",
        json!("idempotencyConflict"),
        json!(refused_key.code()),
        None,
    );

    // Three tickets → three distinct side chats under the coordinator.
    let mut workers = Vec::new();
    for n in 0..3 {
        let saved = ticket(
            &c,
            &format!("three-{n}"),
            "chat-model",
            &format!("Ticket {n}"),
            None,
        )
        .await;
        let created = ok(
            &c,
            "BoardCreateSideChat",
            json!({"requestId":format!("work-{n}"),
            "callerTaskId":main,"ticket":binding(&saved)}),
        )
        .await;
        let worker = created["taskId"].as_str().unwrap().to_owned();
        let worker_identity = identity(&c, &worker).await;
        ev.step(
            &format!("ticket {n}: its own side chat, container frozen under the main"),
            json!({"role":"side","mainParentId":main,"workItemId":saved["workItemId"],
                "container":{"kind":"sideOf","mainTaskId":main},"launch":"created","title":format!("Ticket {n}")}),
            json!({"role":created["role"],"mainParentId":created["mainParentId"],
                "workItemId":created["workItemId"],"container":worker_identity["originalContainer"],
                "launch":created["launch"],"title":row(&chats(&c).await, &worker)["title"]}),
            Some(&worker_identity),
        );
        workers.push(worker);
    }
    let distinct: std::collections::HashSet<_> = workers.iter().collect();
    let badges = ok(
        &c,
        "BoardChatBadges",
        json!({"taskIds":[main, workers[0], workers[1], workers[2]]}),
    )
    .await;
    ev.step(
        "three distinct workers; the coordinator has no working badge",
        json!({"distinct":3,"badgeChats":[workers[0], workers[1], workers[2]],"coordinatorRelated":0}),
        json!({"distinct":distinct.len(),
            "badgeChats":badges["badges"].as_array().unwrap().iter().map(|badge| badge["taskId"].clone()).collect::<Vec<_>>(),
            "coordinatorRelated":badges["related"].as_array().unwrap().len()}),
        Some(&badges),
    );
    let main_identity = identity(&c, &main).await;
    ev.step(
        "the coordinator stays an ordinary main chat",
        json!({"role":"main","purpose":"ordinary","locked":false}),
        json!({"role":main_identity["role"],"purpose":main_identity["purpose"]["kind"],
            "locked":main_identity["parentLocked"]}),
        Some(&main_identity),
    );
    ev.write();
    stop(core, c).await;
}

/// Durable identity of a published chat row (volatile activity fields aside).
fn row_identity(row: &Value) -> Value {
    json!({
        "id": row["id"], "spaceId": row["spaceId"], "config": row["config"],
        "cwd": row["cwd"], "branch": row.get("branch"), "checkoutId": row.get("checkoutId"),
        "archived": row["archived"], "childThread": row.get("childThread"),
        "sideChatParentId": row.get("sideChatParentId"),
        "forkSourceTaskId": row.get("forkSourceTaskId"),
        "harnessSessionId": row.get("harnessSessionId"),
    })
}

fn entry_ids(core: &EngineCore, task: &str) -> Value {
    json!(
        core.doc_host
            .open(task)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
    )
}

fn bindings(core: &EngineCore, task: &str) -> Value {
    let view = core.task_identity.view(task);
    json!(view.map(|view| {
        view.bindings
            .iter()
            .map(|binding| {
                json!([
                    binding.binding_id,
                    binding.generation,
                    binding.provider_task_id
                ])
            })
            .collect::<Vec<_>>()
    }))
}

/// Case 4: converting a running main chat and a side chat keeps everything
/// but the board relationship; duplicates add no second link or send.
#[tokio::test(flavor = "multi_thread")]
async fn conversion_keeps_identity_history_run_draft_and_placement() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut ev = Evidence::new("conversion-in-place");
    let path = project(&c, &core, &f, "chat-model").await;
    let main = main_chat(
        &c,
        "chat-model",
        &path,
        "Main to convert",
        config("fixture", "ask"),
    )
    .await;
    send(&c, &main, "CONVERT_HISTORY").await;
    wait_for("history run", 10, || runs_with(&f, "CONVERT_HISTORY") == 1).await;
    wait_settled(&core, &main).await;
    let draft = luna_proto::ComposerDraft {
        version: luna_proto::COMPOSER_DRAFT_VERSION,
        draft_id: format!("task:{main}"),
        task_id: Some(main.clone()),
        revision: 1,
        text: "UNSENT_DRAFT_TEXT".into(),
        input_items: Vec::new(),
        attachments: Vec::new(),
        appshots: Vec::new(),
        context: Default::default(),
        execution: Value::Null,
        updated_at_ms: 1,
    };
    ok(&c, "SaveComposerDraft", json!({"draft":draft})).await;
    let workbench_before = ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await;
    // An active run that conversion must neither stop nor resend.
    send(&c, &main, "HOLD_CONVERT_RUN").await;
    wait_for("held run", 10, || {
        core.sessions.turn_in_flight(&main) && runs_with(&f, "HOLD_CONVERT_RUN") == 1
    })
    .await;
    // Let the held turn's first streamed text land before the snapshot.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let active_turn = snapshot(&c, &main).await["activeTurnId"].clone();
    let rows = chats(&c).await;
    let before = json!({
        "row": row_identity(row(&rows, &main)),
        "entries": entry_ids(&core, &main),
        "bindings": bindings(&core, &main),
        "draft": ok(&c, "GetComposerDraft", json!({"taskId":main})).await["draft"],
        "workbench": workbench_before["root"],
    });
    let runs_before = runs(&f).len();

    let convert = json!({"requestId":"convert-main","taskId":main,"description":"Agreed brief: finish the importer."});
    let receipt = ok(&c, "BoardConvertChat", convert.clone()).await;
    ev.step(
        "same chat becomes the working chat in place",
        json!({"taskId":main,"operation":"convertChat","role":"working","container":{"kind":"mainChat"},
            "purpose":"ticketWorker","title":"Main to convert","running":true,"activeTurnId":active_turn,
            "navigation":{"kind":"mainChat","taskId":main}}),
        json!({"taskId":receipt["mutation"]["taskId"],"operation":receipt["mutation"]["operation"],
            "role":receipt["link"]["role"],"container":receipt["link"]["originalContainer"],
            "purpose":receipt["identity"]["purpose"]["kind"],"title":receipt["mutation"]["item"]["title"],
            "running":receipt["run"]["running"],"activeTurnId":receipt["run"]["activeTurnId"],
            "navigation":receipt["mutation"]["navigation"]}),
        Some(&receipt),
    );
    let rows = chats(&c).await;
    let after = json!({
        "row": row_identity(row(&rows, &main)),
        "entries": entry_ids(&core, &main),
        "bindings": bindings(&core, &main),
        "draft": ok(&c, "GetComposerDraft", json!({"taskId":main})).await["draft"],
        "workbench": ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await["root"],
    });
    ev.unchanged(
        "id, history, binding, settings, draft and placement",
        &before,
        &after,
    );
    ev.step(
        "nothing sent, the run keeps going",
        json!({"runs": runs_before, "held": 1, "inFlight": true}),
        json!({"runs": runs(&f).len(), "held": runs_with(&f, "HOLD_CONVERT_RUN"),
            "inFlight": core.sessions.turn_in_flight(&main)}),
        None,
    );
    let replay = ok(&c, "BoardConvertChat", convert).await;
    ev.step(
        "retry returns the same receipt",
        receipt["mutation"].clone(),
        replay["mutation"].clone(),
        None,
    );
    let duplicate = refused(
        &c,
        "BoardConvertChat",
        json!({"requestId":"convert-main-again","taskId":main,
        "description":"Again"}),
    )
    .await;
    let detail = read(&c, receipt["mutation"]["workItemId"].as_str().unwrap()).await;
    ev.step(
        "a second conversion is refused; exactly one working link",
        json!({"code":"notConvertible","workingLinks":1,"workingChat":main}),
        json!({"code":duplicate.code(),"workingLinks":working_links(&detail).len(),
            "workingChat":detail["view"]["workingChat"]["taskId"]}),
        Some(&detail),
    );
    interrupt(&c, &main).await;
    wait_settled(&core, &main).await;

    // A side chat converts in place under its existing main.
    let coordinator = main_chat(
        &c,
        "chat-model",
        &path,
        "Coordinator",
        config("fixture", "ask"),
    )
    .await;
    let side = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"side-to-convert","callerTaskId":coordinator,
        "title":"Side discussion"}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let side_before = json!([
        container_identity(&identity(&c, &side).await),
        row_identity(row(&chats(&c).await, &side))
    ]);
    let receipt = ok(
        &c,
        "BoardConvertChat",
        json!({"requestId":"convert-side","taskId":side,
        "description":"Side brief"}),
    )
    .await;
    ev.step(
        "side chat keeps its main parent and gets a frozen SideOf container",
        json!({"taskId":side,"container":{"kind":"sideOf","mainTaskId":coordinator},"role":"side",
            "mainParentId":coordinator,"navigation":{"kind":"sideChat","mainTaskId":coordinator,"sideTaskId":side}}),
        json!({"taskId":receipt["mutation"]["taskId"],"container":receipt["link"]["originalContainer"],
            "role":receipt["identity"]["role"],"mainParentId":receipt["identity"]["mainParentId"],
            "navigation":receipt["mutation"]["navigation"]}),
        Some(&receipt),
    );
    ev.unchanged(
        "side chat placement and row unchanged",
        &side_before,
        &json!([
            container_identity(&identity(&c, &side).await),
            row_identity(row(&chats(&c).await, &side))
        ]),
    );
    let not_a_chat = refused(
        &c,
        "BoardConvertChat",
        json!({"requestId":"convert-nothing",
        "taskId":uuid::Uuid::new_v4().to_string(),"description":"x"}),
    )
    .await;
    ev.step(
        "an unknown chat is not convertible",
        json!("invalidInput"),
        json!(not_a_chat.code()),
        None,
    );
    ev.write();
    stop(core, c).await;
}

/// Cases 6, 7, 15: Go to chat from durable records after closing tabs and a
/// restart; one rename keeps ticket and working chat agreed in both
/// directions while parent, source and attempt titles stay.
#[tokio::test(flavor = "multi_thread")]
async fn navigation_survives_closed_tabs_and_restart_and_titles_stay_in_sync() {
    let f = fixture();
    let mut ev = Evidence::new("navigation-and-title-sync");
    let (core, c) = boot(&f);
    let path = project(&c, &core, &f, "chat-model").await;
    let main = main_chat(
        &c,
        "chat-model",
        &path,
        "Parent coordinator",
        config("fixture", "ask"),
    )
    .await;
    let source = main_chat(
        &c,
        "chat-model",
        &path,
        "Source talk",
        config("fixture", "ask"),
    )
    .await;
    let saved = ticket(
        &c,
        "nav-ticket",
        "chat-model",
        "Render the chart",
        Some(&source),
    )
    .await;
    let item = saved["workItemId"].as_str().unwrap().to_owned();
    let worker = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"nav-worker","callerTaskId":main,
        "ticket":binding(&saved)}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let expected = json!({"kind":"sideChat","mainTaskId":main,"sideTaskId":worker});

    // Close every tab of the worker in the parent's workbench.
    let profile = ok(&c, "EngineInfo", json!({})).await["profileId"].clone();
    let layout = ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await;
    if layout["root"].to_string().contains(&worker) {
        let closed = ok(
            &c,
            "MutateChatWorkbench",
            json!({"mainChatId":main,"profileId":profile,
            "expectedRevision":layout["revision"],"commandId":"close-worker",
            "command":{"op":"closeTab","chatId":worker}}),
        )
        .await;
        assert_eq!(closed["conflict"], false, "{closed}");
    }
    let layout = ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await;
    let by_ticket = ok(&c, "BoardNavigation", json!({"workItemId":item})).await;
    let by_chat = ok(&c, "BoardNavigation", json!({"taskId":worker})).await;
    ev.step(
        "Go to chat with the tab closed",
        json!({"tabOpen":false,"byTicket":expected,"byChat":expected,"runs":0}),
        json!({"tabOpen":layout["root"].to_string().contains(&worker),"byTicket":by_ticket,"byChat":by_chat,
            "runs":runs(&f).len()}),
        Some(&layout),
    );
    let identity_before = identity(&c, &worker).await;
    stop(core, c).await;

    let (core, c) = boot(&f);
    let by_ticket = ok(&c, "BoardNavigation", json!({"workItemId":item})).await;
    ev.step(
        "Go to chat after restart, nothing relaunched",
        json!({"byTicket":expected,"runs":0}),
        json!({"byTicket":by_ticket,"runs":runs(&f).len()}),
        None,
    );
    ev.unchanged(
        "canonical identity after restart",
        &identity_before,
        &identity(&c, &worker).await,
    );

    // Ticket rename → working chat follows; parent and source keep theirs.
    let current = read(&c, &item).await["view"]["item"]["revision"].clone();
    ok(
        &c,
        "BoardUpdate",
        json!({"requestId":"rename-ticket","workItemId":item,
        "expectedRevision":current,"changes":{"title":"Render the revenue chart"}}),
    )
    .await;
    let rows = chats(&c).await;
    ev.step(
        "ticket rename renames the working chat only",
        json!({"worker":"Render the revenue chart","parent":"Parent coordinator","source":"Source talk"}),
        json!({"worker":row(&rows, &worker)["title"],"parent":row(&rows, &main)["title"],
            "source":row(&rows, &source)["title"]}),
        None,
    );
    // Working chat rename (the chat header / Mutate renameChat) → ticket follows.
    let renamed = ok(
        &c,
        "Mutate",
        json!({"op":"renameChat","chatId":worker,"title":"Chart with legend"}),
    )
    .await;
    assert_eq!(renamed["receipt"]["status"], "applied", "{renamed}");
    let detail = read(&c, &item).await;
    let events = ok(&c, "BoardEvents", json!({"workItemId":item,"limit":100})).await;
    let renames = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"]["kind"] == "renamed")
        .count();
    let rows = chats(&c).await;
    ev.step(
        "chat rename renames the ticket in one operation",
        json!({"ticket":"Chart with legend","worker":"Chart with legend","parent":"Parent coordinator",
            "source":"Source talk","renamedEvents":1}),
        json!({"ticket":detail["view"]["item"]["title"],"worker":row(&rows, &worker)["title"],
            "parent":row(&rows, &main)["title"],"source":row(&rows, &source)["title"],
            "renamedEvents":renames}),
        Some(&detail),
    );
    // Renaming the source discussion or a historical attempt never renames
    // the ticket.
    let attempt = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"nav-attempt","callerTaskId":main,
        "title":"Earlier attempt"}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let revision = read(&c, &item).await["view"]["item"]["revision"].clone();
    ok(
        &c,
        "BoardLinkChat",
        json!({"requestId":"link-attempt","workItemId":item,"expectedRevision":revision,
        "taskId":attempt,"role":"attempt"}),
    )
    .await;
    for (task, title) in [(&attempt, "Attempt renamed"), (&source, "Source renamed")] {
        let result = ok(
            &c,
            "Mutate",
            json!({"op":"renameChat","chatId":task,"title":title}),
        )
        .await;
        assert_eq!(result["receipt"]["status"], "applied", "{result}");
    }
    let detail = read(&c, &item).await;
    let badges = ok(
        &c,
        "BoardChatBadges",
        json!({"taskIds":[worker, attempt, source]}),
    )
    .await;
    let mut related = badges["related"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| json!([b["taskId"], b["role"]]))
        .collect::<Vec<_>>();
    related.sort_by_key(|pair| pair[1].as_str().unwrap_or_default().to_owned());
    ev.step(
        "attempt and source titles are their own; badges stay truthful",
        json!({"ticket":"Chart with legend","badges":[[worker,"working"]],
            "related":[[attempt,"attempt"],[source,"source"]]}),
        json!({"ticket":detail["view"]["item"]["title"],
            "badges":badges["badges"].as_array().unwrap().iter().map(|b| json!([b["taskId"], b["role"]])).collect::<Vec<_>>(),
            "related":related}),
        Some(&badges),
    );
    ev.write();
    stop(core, c).await;
}

/// Cases 11, 15, 18: a side chat — fresh child or context fork, also when
/// it is shown as its own workbench or the selected tab — cannot nest, fork
/// or copy its conversation through any UI-equivalent RPC or tool path.
#[tokio::test(flavor = "multi_thread")]
async fn side_chats_cannot_nest_or_fork_through_rpc_or_tools() {
    let f = fixture();
    let (core, c) = boot(&f);
    let service = core.rpc_service();
    let mut ev = Evidence::new("side-depth-and-context-restriction");
    let path = project(&c, &core, &f, "chat-model").await;
    let main = main_chat(&c, "chat-model", &path, "Main", config("fixture", "ask")).await;
    send(&c, &main, "RESTRICTION_HISTORY").await;
    wait_for("main history", 10, || {
        runs_with(&f, "RESTRICTION_HISTORY") == 1
    })
    .await;
    wait_settled(&core, &main).await;
    let fresh = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"restrict-fresh","callerTaskId":main,
        "title":"Fresh side"}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let forked_id = uuid::Uuid::new_v4().to_string();
    let forked = ok(
        &c,
        "CreateSideChat",
        json!({"parentId":main,"requestId":forked_id,"forceSnapshot":true}),
    )
    .await;
    let forked = forked["meta"]["id"].as_str().unwrap().to_owned();

    // The side chat as its own workbench (a promoted view) and as the
    // selected tab of its main's workbench: presentation only.
    let profile = ok(&c, "EngineInfo", json!({})).await["profileId"].clone();
    let before_views = json!([identity(&c, &fresh).await, identity(&c, &forked).await]);
    ok(&c, "GetChatWorkbench", json!({"mainChatId":fresh})).await;
    let layout = ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await;
    let focused = ok(
        &c,
        "MutateChatWorkbench",
        json!({"mainChatId":main,"profileId":profile,
        "expectedRevision":layout["revision"],"commandId":"focus-side",
        "command":{"op":"openTab","tab":{"chatId":fresh}}}),
    )
    .await;
    assert_eq!(focused["conflict"], false, "{focused}");
    let after_views = json!([identity(&c, &fresh).await, identity(&c, &forked).await]);
    ev.unchanged(
        "canonical identity ignores workbenches and selected tabs",
        &before_views,
        &after_views,
    );
    ev.step(
        "both are side chats of the main with sibling-only creation",
        json!([
            ["side", main, false, false, true],
            ["side", main, false, false, true]
        ]),
        json!(
            after_views
                .as_array()
                .unwrap()
                .iter()
                .map(|identity| json!([
                    identity["role"],
                    identity["mainParentId"],
                    identity["permitted"]["createSideFresh"],
                    identity["permitted"]["createSideWithContext"],
                    identity["permitted"]["createSiblingFresh"]
                ]))
                .collect::<Vec<_>>()
        ),
        Some(&after_views),
    );

    let chats_before = chats(&c).await.len();
    let runs_before = runs(&f).len();
    for (label, side) in [("fresh", &fresh), ("fork", &forked)] {
        let attempt = uuid::Uuid::new_v4().to_string();
        let mut observed = serde_json::Map::new();
        let nested = refused(
            &c,
            "CreateSideChat",
            json!({"parentId":side,"requestId":attempt,"forceSnapshot":true}),
        )
        .await;
        observed.insert("createSideChat".into(), refusal(&nested));
        let tab = refused(
            &c,
            "CreateChildTab",
            json!({"parentTaskId":side,"idempotencyKey":format!("tab-{label}")}),
        )
        .await;
        observed.insert("createChildTab".into(), refusal(&tab));
        let context = refused(
            &c,
            "BoardCreateSideChat",
            json!({"requestId":format!("ctx-{label}"),
            "callerTaskId":side,"context":{"kind":"withContext"}}),
        )
        .await;
        observed.insert("boardWithContext".into(), refusal(&context));
        let fork_target = uuid::Uuid::new_v4().to_string();
        let snap = snapshot(&c, side).await;
        let fork = command(&c, side, &format!("fork-{label}"), json!({"kind":"forkTask","newTaskId":fork_target,
            "boundary":"tip","review":{"taskRevision":snap["taskRevision"],"bindingGeneration":snap["bindingGeneration"].as_u64().unwrap_or(0),
            "providerSessionId":"","cwd":path}})).await;
        observed.insert("messageFork".into(), json!({"status":fork["status"],
            "refused":fork["reason"]["message"].as_str().unwrap_or_default().contains("cannot fork")}));
        let forged = uuid::Uuid::new_v4().to_string();
        let lineage = ok(&c, "ExecuteTaskCommand", json!({"commandId":format!("forged-{label}"),"taskId":forged,
            "expectedTaskRevision":0,"intent":{"kind":"createChat","spaceId":"chat-model","cwd":path,
            "config":config("fixture","ask"),"lineage":{"sourceTaskId":side}}})).await;
        observed.insert(
            "forgedLineage".into(),
            json!({"status":lineage["status"],
            "created":core.workspace.chat(&forged).unwrap().is_some()}),
        );
        let portable = call(&c, "PreviewContextTransfer", json!({"kind":"portableFork","sourceTaskId":side,
            "targetTaskId":uuid::Uuid::new_v4().to_string(),"newTarget":{"config":config("fixture","ask")}})).await;
        observed.insert(
            "portableCopy".into(),
            json!(portable.is_err_and(|error| error.to_string().contains("cannot fork"))),
        );
        // Tool paths: the create_thread preflight (before any approval card)
        // and manage_workspace forkThread, also forking the main as a proxy.
        let child = serde_json::from_value::<luna_proto::CreateChildThreadRequest>(json!({
            "prompt":"nested helper","idempotencyKey":format!("tool-{label}")}))
        .unwrap();
        let tool = service
            .child_creation_snapshot(&child, &authority(&core, side, "chat-model"))
            .await;
        observed.insert(
            "createThreadTool".into(),
            json!(
                tool.err()
                    .is_some_and(|error| format!("{error:?}").contains("invalidRole"))
            ),
        );
        for (name, caller, target) in [
            ("forkThreadTool", side.as_str(), side.as_str()),
            ("forkMainAsProxy", side.as_str(), main.as_str()),
        ] {
            let request = serde_json::from_value(json!({"action":"forkThread","taskIds":[target],
                "idempotencyKey":format!("{name}-{label}")}))
            .unwrap();
            let result = service
                .prepare_workspace(request, &authority(&core, caller, "chat-model"))
                .await;
            observed.insert(
                name.into(),
                json!(
                    result
                        .err()
                        .is_some_and(|error| format!("{error:?}").contains("invalidRole"))
                ),
            );
        }
        let request = serde_json::from_value(json!({"action":"forkThread","taskIds":[side],
            "idempotencyKey":format!("main-forks-side-{label}")}))
        .unwrap();
        let result = service
            .prepare_workspace(request, &authority(&core, &main, "chat-model"))
            .await;
        observed.insert(
            "mainForksSide".into(),
            json!(
                result
                    .err()
                    .is_some_and(|error| format!("{error:?}").contains("invalidRole"))
            ),
        );
        let refusal_ok = json!({"code":"invalidRole","alternatives":true});
        ev.step(
            &format!("{label} side chat: every nesting/fork path refused"),
            json!({"createSideChat":refusal_ok,"createChildTab":refusal_ok,"boardWithContext":refusal_ok,
                "messageFork":{"status":"failed","refused":true},
                "forgedLineage":{"status":"failed","created":false},"portableCopy":true,
                "createThreadTool":true,"forkThreadTool":true,"forkMainAsProxy":true,"mainForksSide":true}),
            Value::Object(observed),
            None,
        );
    }
    ev.step(
        "no chat created, nothing sent",
        json!({"chats":chats_before,"runs":runs_before}),
        json!({"chats":chats(&c).await.len(),"runs":runs(&f).len()}),
        None,
    );
    // The allowed alternative works from the side chat: a fresh sibling.
    let sibling = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"restrict-sibling","callerTaskId":forked}),
    )
    .await;
    ev.step(
        "the allowed alternative: a fresh sibling under the same main",
        json!({"role":"side","mainParentId":main,"creator":{"taskId":forked,"relation":"createdBy"}}),
        json!({"role":sibling["role"],"mainParentId":sibling["mainParentId"],"creator":sibling["creator"]}),
        Some(&sibling),
    );
    ev.unchanged(
        "side identities unchanged after every attempt",
        &after_views,
        &json!([identity(&c, &fresh).await, identity(&c, &forked).await]),
    );
    ev.write();
    drop(service);
    stop(core, c).await;
}

/// Cases 12, 13, 14, 16: side → fresh sibling (creator's model and access,
/// main parent, no result routing), side → new main handoff (sidebar root,
/// brief only, one send only when started), the new main owns its own side
/// chats, and no self-promotion or create-root-then-adopt sequence moves
/// the side chat.
#[tokio::test(flavor = "multi_thread")]
async fn side_handoff_sibling_and_self_promotion_attempts() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut ev = Evidence::new("side-handoff-and-sibling");
    let path = project(&c, &core, &f, "chat-model").await;
    let main = main_chat(
        &c,
        "chat-model",
        &path,
        "Full-access main",
        config("fixture", "full"),
    )
    .await;
    // The side creator picked another model and a lower access level.
    let side = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"creator-side","callerTaskId":main,
        "title":"Side creator","execution":{"model":"fixture-b","permissionMode":"ask"}}),
    )
    .await;
    let side = side["taskId"].as_str().unwrap().to_owned();
    send(&c, &side, "SIDE_PRIVATE_HISTORY").await;
    wait_for("side history", 10, || {
        runs_with(&f, "SIDE_PRIVATE_HISTORY") == 1
    })
    .await;
    wait_settled(&core, &side).await;
    let side_before = json!([
        identity(&c, &side).await,
        row_identity(row(&chats(&c).await, &side))
    ]);
    let main_before = row_identity(row(&chats(&c).await, &main));

    // Side → sibling.
    let sibling = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"sibling","callerTaskId":side,
        "prompt":"SIBLING_BRIEF","start":true}),
    )
    .await;
    let sibling_id = sibling["taskId"].as_str().unwrap().to_owned();
    wait_for("sibling run", 10, || runs_with(&f, "SIBLING_BRIEF") == 1).await;
    let rows = chats(&c).await;
    let sibling_row = row(&rows, &sibling_id);
    let sibling_run = runs(&f)
        .into_iter()
        .find(|run| run.prompt.contains("SIBLING_BRIEF"))
        .unwrap();
    ev.step(
        "sibling: same main, side creator provenance, creator's model and access",
        json!({"role":"side","mainParentId":main,"childParent":main,
            "creator":{"taskId":side,"relation":"createdBy"},"model":"fixture-b","runModel":"fixture-b",
            "access":"ask","reportsTo":false,"inheritedHistory":false}),
        json!({"role":sibling["role"],"mainParentId":sibling["mainParentId"],
            "childParent":sibling_row.pointer("/childThread/parentTaskId"),
            "creator":sibling["creator"],"model":sibling_row["config"]["model"],
            "runModel":sibling_run.model,"access":sibling_row["config"]["effectivePermissionMode"],
            "reportsTo":sibling_row.pointer("/childThread/reportResults"),
            "inheritedHistory":sibling_run.prompt.contains("SIDE_PRIVATE_HISTORY")}),
        Some(sibling_row),
    );
    let mut parent_claim = json!({"requestId":"sibling-with-parent","callerTaskId":side});
    parent_claim["mainTaskId"] = json!(sibling_id);
    let rejected = call(&c, "BoardCreateSideChat", parent_claim).await;
    ev.step(
        "a caller-supplied parent is not a request field",
        json!(true),
        json!(rejected.is_err_and(|error| error.to_string().contains("mainTaskId"))),
        None,
    );

    // Side → new main handoff: brief required, prepare without sending.
    let missing = refused(
        &c,
        "BoardCreateMainChat",
        json!({"requestId":"handoff-no-brief","projectId":"chat-model",
        "fromTaskId":side}),
    )
    .await;
    ev.step(
        "a side handoff needs a written brief",
        json!("invalidInput"),
        json!(missing.code()),
        None,
    );
    let runs_before = runs(&f).len();
    let prepared = ok(&c, "BoardCreateMainChat", json!({"requestId":"handoff","projectId":"chat-model",
        "fromTaskId":side,"title":"Follow-up work","handoffBrief":"HANDOFF_BRIEF goal: finish; done: parser; next: tests"})).await;
    let handoff = prepared["taskId"].as_str().unwrap().to_owned();
    let rows = chats(&c).await;
    let handoff_identity = identity(&c, &handoff).await;
    ev.step(
        "new independent sidebar root with a handoff source, nothing sent",
        json!({"role":"main","mainParentId":null,"contextOrigin":"handoffBrief","launch":"created",
            "creator":{"taskId":side,"relation":"createdBy"},
            "source":{"taskId":side,"relation":"handedOffFrom"},"sidebarRoot":true,"entries":0,
            "runs":runs_before,"title":"Follow-up work","access":"ask","locked":false,
            "navigation":{"kind":"mainChat","taskId":handoff}}),
        json!({"role":prepared["role"],"mainParentId":prepared.get("mainParentId"),
            "contextOrigin":prepared["contextOrigin"],"launch":prepared["launch"],
            "creator":prepared["creator"],"source":handoff_identity["source"],
            "sidebarRoot":sidebar_root(row(&rows, &handoff)),
            "entries":entry_ids(&core, &handoff).as_array().unwrap().len(),"runs":runs(&f).len(),
            "title":row(&rows, &handoff)["title"],
            "access":row(&rows, &handoff)["config"]["effectivePermissionMode"],
            "locked":handoff_identity["parentLocked"],"navigation":prepared["navigation"]}),
        Some(&handoff_identity),
    );
    // Start there: exactly one send, also on retry.
    let start = json!({"requestId":"handoff-start","projectId":"chat-model","fromTaskId":side,
        "handoffBrief":"HANDOFF_START_BRIEF continue with the tests","start":true});
    let started = ok(&c, "BoardCreateMainChat", start.clone()).await;
    let started_id = started["taskId"].as_str().unwrap().to_owned();
    wait_for("handoff run", 10, || {
        runs_with(&f, "HANDOFF_START_BRIEF") == 1
    })
    .await;
    wait_settled(&core, &started_id).await;
    let replay = ok(&c, "BoardCreateMainChat", start).await;
    let handoff_run = runs(&f)
        .into_iter()
        .find(|run| run.prompt.contains("HANDOFF_START_BRIEF"))
        .unwrap();
    ev.step(
        "start sends the brief once and nothing else",
        json!({"launch":"queued","replay":started_id,"sends":1,"inheritedHistory":false}),
        json!({"launch":started["launch"],"replay":replay["taskId"],
            "sends":runs_with(&f, "HANDOFF_START_BRIEF"),
            "inheritedHistory":handoff_run.prompt.contains("SIDE_PRIVATE_HISTORY")}),
        None,
    );

    // The new main owns ordinary main-chat creation; the old main gains
    // nothing over it.
    let owned_fresh = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"owned-fresh","callerTaskId":handoff}),
    )
    .await;
    let owned_context = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"owned-context","callerTaskId":handoff,
        "context":{"kind":"withContext"}}),
    )
    .await;
    ev.step(
        "the new main creates fresh and with-context side chats under itself",
        json!([
            ["side", handoff, "fresh"],
            ["side", handoff, "contextSnapshot"]
        ]),
        json!([
            [
                owned_fresh["role"],
                owned_fresh["mainParentId"],
                owned_fresh["contextOrigin"]
            ],
            [
                owned_context["role"],
                owned_context["mainParentId"],
                owned_context["contextOrigin"]
            ]
        ]),
        None,
    );

    // Self-promotion / self-move / create-root-then-adopt.
    let reuse_id = call(
        &c,
        "CreateSideChat",
        json!({"parentId":handoff,"requestId":side,"forceSnapshot":true}),
    )
    .await;
    let forged = command(
        &c,
        &side,
        "adopt-lineage",
        json!({"kind":"createChat","spaceId":"chat-model",
        "cwd":path,"lineage":{"parentTaskId":handoff}}),
    )
    .await;
    let target = ticket(
        &c,
        "adopt-target",
        "chat-model",
        "Owned by the new main",
        None,
    )
    .await;
    let new_worker = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"adopt-worker","callerTaskId":handoff,
        "ticket":binding(&target)}),
    )
    .await;
    let revision =
        read(&c, target["workItemId"].as_str().unwrap()).await["view"]["item"]["revision"].clone();
    let attempt = refused(
        &c,
        "BoardLinkChat",
        json!({"requestId":"adopt-attempt","workItemId":target["workItemId"],
        "expectedRevision":revision,"taskId":side,"role":"attempt"}),
    )
    .await;
    let working = refused(
        &c,
        "BoardLinkChat",
        json!({"requestId":"adopt-working","workItemId":target["workItemId"],
        "expectedRevision":revision,"taskId":side,"role":"working"}),
    )
    .await;
    ev.step(
        "no request moves the side chat under the new main",
        json!({"reuseOwnId":true,"forgedLineageApplied":false,"attempt":"parentLocked",
            "working":"invalidRole","newWorkerParent":handoff}),
        json!({"reuseOwnId":reuse_id.is_err(),
            "forgedLineageApplied":forged["status"] == "applied"
                && core.task_identity.view(&side).is_some_and(|view| view.task.lineage.parent_task_id.as_deref() == Some(handoff.as_str())),
            "attempt":attempt.code(),"working":working.code(),
            "newWorkerParent":new_worker["mainParentId"]}),
        Some(&forged),
    );
    // Converting the side chat freezes its existing container; another main
    // asking for that ticket gets the side chat where it already lives.
    let converted = ok(
        &c,
        "BoardConvertChat",
        json!({"requestId":"convert-creator","taskId":side,
        "description":"Side ticket"}),
    )
    .await;
    let reused = ok(&c, "BoardCreateSideChat", json!({"requestId":"adopt-converted","callerTaskId":handoff,
        "ticket":{"workItemId":converted["mutation"]["workItemId"],"expectedRevision":converted["mutation"]["revision"]}})).await;
    ev.step(
        "conversion keeps the original main; another main reuses in place",
        json!({"container":{"kind":"sideOf","mainTaskId":main},"reused":true,"taskId":side,
            "navigation":{"kind":"sideChat","mainTaskId":main,"sideTaskId":side}}),
        json!({"container":converted["link"]["originalContainer"],"reused":reused["reused"],
            "taskId":reused["taskId"],"navigation":reused["navigation"]}),
        Some(&reused),
    );
    let side_after = json!([
        identity(&c, &side).await,
        row_identity(row(&chats(&c).await, &side))
    ]);
    ev.unchanged(
        "side chat placement, row and parent unchanged",
        &json!([container_identity(&side_before[0]), side_before[1]]),
        &json!([container_identity(&side_after[0]), side_after[1]]),
    );
    ev.unchanged(
        "the original main is untouched",
        &main_before,
        &row_identity(row(&chats(&c).await, &main)),
    );
    ev.write();
    stop(core, c).await;
}

fn worker_link(detail: &Value) -> Value {
    let link = working_links(detail)
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    json!({"id":link["id"],"taskId":link["taskId"],"container":link["originalContainer"],
        "chatRole":link["chatRole"]})
}

/// Case 17 and the fixed-parent acceptance: idle and active ticket workers
/// keep their original parent against second-parent requests, Attempt
/// links from another container, new-main requests and project moves; the
/// worker is reused without a second first message.
#[tokio::test(flavor = "multi_thread")]
async fn fixed_parent_rejects_idle_and_active_reparenting() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut ev = Evidence::new("fixed-original-parent");
    let path = project(&c, &core, &f, "chat-model").await;
    project(&c, &core, &f, "elsewhere").await;
    let original = main_chat(
        &c,
        "chat-model",
        &path,
        "Original parent",
        config("fixture", "ask"),
    )
    .await;
    let other = main_chat(
        &c,
        "chat-model",
        &path,
        "Other main",
        config("fixture", "ask"),
    )
    .await;
    let saved = ticket(&c, "fixed-ticket", "chat-model", "Keep my parent", None).await;
    let item = saved["workItemId"].as_str().unwrap().to_owned();
    let worker = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"fixed-worker","callerTaskId":original,
        "ticket":binding(&saved)}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_side = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"other-side","callerTaskId":other}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let canonical = json!({"kind":"sideChat","mainTaskId":original,"sideTaskId":worker});
    let before = json!({"link":worker_link(&read(&c, &item).await),
        "row":row_identity(row(&chats(&c).await, &worker)),
        "identity":container_identity(&identity(&c, &worker).await)});

    for phase in ["idle", "active"] {
        if phase == "active" {
            send(&c, &worker, "HOLD_FIXED_WORKER").await;
            wait_for("worker active", 10, || {
                core.sessions.turn_in_flight(&worker)
            })
            .await;
        }
        let runs_before = runs(&f).len();
        let revision = read(&c, &item).await["view"]["item"]["revision"].clone();
        let second = ok(
            &c,
            "BoardCreateSideChat",
            json!({"requestId":format!("second-{phase}"),
            "callerTaskId":other,"ticket":{"workItemId":item,"expectedRevision":revision}}),
        )
        .await;
        let as_main = ok(
            &c,
            "BoardCreateMainChat",
            json!({"requestId":format!("as-main-{phase}"),
            "projectId":"chat-model","ticket":{"workItemId":item,"expectedRevision":revision},
            "execution":{"harness":"mock","model":"fixture"}}),
        )
        .await;
        let attempt = refused(
            &c,
            "BoardLinkChat",
            json!({"requestId":format!("attempt-{phase}"),
            "workItemId":item,"expectedRevision":revision,"taskId":other_side,"role":"attempt"}),
        )
        .await;
        let moved = command(
            &c,
            &worker,
            &format!("move-{phase}"),
            json!({"kind":"moveProject","destinationSpaceId":"elsewhere"}),
        )
        .await;
        ev.step(
            &format!("{phase} worker: reused in place, never moved"),
            json!({"second":[worker, true, canonical],"asMain":[worker, true, canonical],
                "attempt":["parentLocked", original],"moved":false,"newRuns":0,"held":usize::from(phase == "active")}),
            json!({"second":[second["taskId"], second["reused"], second["navigation"]],
                "asMain":[as_main["taskId"], as_main["reused"], as_main["navigation"]],
                "attempt":[attempt.code(), match &attempt { BoardError::ParentLocked { original_parent_id, .. } => json!(original_parent_id), _ => Value::Null }],
                "moved":moved["status"] == "applied","newRuns":runs(&f).len() - runs_before,
                "held":runs_with(&f, "HOLD_FIXED_WORKER")}),
            Some(&moved),
        );
        let after = json!({"link":worker_link(&read(&c, &item).await),
            "row":row_identity(row(&chats(&c).await, &worker)),
            "identity":container_identity(&identity(&c, &worker).await)});
        // A run changes the provider session id, never the relationship.
        let mut stable_before = before.clone();
        let mut stable_after = after.clone();
        for value in [&mut stable_before, &mut stable_after] {
            value["row"]["harnessSessionId"] = Value::Null;
        }
        ev.unchanged(
            &format!("{phase}: link, row and container unchanged"),
            &stable_before,
            &stable_after,
        );
    }
    ev.step(
        "the active run kept going; no launches were made",
        json!({"inFlight":true,"launches":0}),
        json!({"inFlight":core.sessions.turn_in_flight(&worker),
            "launches":read(&c, &item).await["launches"].as_array().unwrap().len()}),
        None,
    );
    interrupt(&c, &worker).await;
    wait_settled(&core, &worker).await;
    ev.write();
    stop(core, c).await;
}

/// Case 10: an archived parent leaves the worker reachable directly; the
/// parent reference is kept (not rewritten) and nothing restarts.
#[tokio::test(flavor = "multi_thread")]
async fn archived_parent_keeps_the_worker_navigable() {
    let f = fixture();
    let (core, c) = boot(&f);
    let mut ev = Evidence::new("unavailable-parent");
    let path = project(&c, &core, &f, "chat-model").await;
    let parent = main_chat(
        &c,
        "chat-model",
        &path,
        "Parent to archive",
        config("fixture", "ask"),
    )
    .await;
    let saved = ticket(
        &c,
        "orphan-ticket",
        "chat-model",
        "Survive the parent",
        None,
    )
    .await;
    let item = saved["workItemId"].as_str().unwrap().to_owned();
    let worker = ok(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"orphan-worker","callerTaskId":parent,
        "ticket":binding(&saved)}),
    )
    .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let link_before = worker_link(&read(&c, &item).await);
    let archived = ok(
        &c,
        "Mutate",
        json!({"op":"setChatArchived","chatId":parent,"archived":true}),
    )
    .await;
    assert_eq!(archived["receipt"]["status"], "applied", "{archived}");
    let direct = json!({"kind":"direct","taskId":worker,"originalParentId":parent});
    let worker_identity = identity(&c, &worker).await;
    let detail = read(&c, &item).await;
    let rows = chats(&c).await;
    ev.step(
        "the worker opens directly; the parent reference is kept",
        json!({"byTicket":direct,"byChat":direct,"parentUnavailable":true,"mainParentId":parent,
            "sibling":false,"workerArchived":false,"parentAvailable":false,"runs":0}),
        json!({"byTicket":ok(&c, "BoardNavigation", json!({"workItemId":item})).await,
            "byChat":ok(&c, "BoardNavigation", json!({"taskId":worker})).await,
            "parentUnavailable":worker_identity["parentUnavailable"],
            "mainParentId":worker_identity["mainParentId"],
            "sibling":worker_identity["permitted"]["createSiblingFresh"],
            "workerArchived":row(&rows, &worker)["archived"],
            "parentAvailable":detail["view"]["workingChat"]["parentAvailable"],"runs":runs(&f).len()}),
        Some(&worker_identity),
    );
    ev.unchanged(
        "the frozen link is not rewritten",
        &link_before,
        &worker_link(&detail),
    );
    let refused_sibling = refused(
        &c,
        "BoardCreateSideChat",
        json!({"requestId":"orphan-sibling",
        "callerTaskId":worker}),
    )
    .await;
    ev.step(
        "no sibling under an unavailable main; a new main is the alternative",
        json!({"code":"invalidRole","alternatives":true}),
        refusal(&refused_sibling),
        None,
    );
    let restored = ok(
        &c,
        "Mutate",
        json!({"op":"setChatArchived","chatId":parent,"archived":false}),
    )
    .await;
    assert_eq!(restored["receipt"]["status"], "applied", "{restored}");
    ev.step(
        "unarchiving restores the parent workbench view",
        json!({"kind":"sideChat","mainTaskId":parent,"sideTaskId":worker}),
        ok(&c, "BoardNavigation", json!({"workItemId":item})).await,
        None,
    );
    ev.write();
    stop(core, c).await;
}

/// The lineage evidence migration must preserve verbatim.
fn legacy_evidence(core: &EngineCore, c_rows: &[Value], task: &str) -> Value {
    let row = row(c_rows, task);
    let child = &row["childThread"];
    json!({
        "id": row["id"], "harness": row["config"]["harness"], "model": row["config"]["model"],
        "access": row["config"]["effectivePermissionMode"], "cwd": row["cwd"],
        "parent": child["parentTaskId"], "creation": child["creationCommandId"],
        "ceiling": child["permissionCeiling"], "sandboxCeiling": child.get("sandboxCeiling"),
        "provisioning": child["provisioning"], "initialMessage": child["initialMessageId"],
        "initialPrompt": child["initialPrompt"], "reports": child["reportResults"],
        "continues": child["continueParent"],
        "lineage": core.task_identity.view(task).map(|view| json!(view.task.lineage)),
    })
}

fn delivered_outcomes(rows: &[Value], task: &str) -> Value {
    json!(
        row(rows, task)["childThread"]["outcomes"]
            .as_array()
            .map(|outcomes| outcomes
                .iter()
                .filter(|outcome| outcome["delivered"] == true)
                .map(|outcome| outcome["id"].clone())
                .collect::<Vec<_>>())
    )
}

/// Case 9: legacy nested children (made before the depth rule by the
/// trusted fixture seeder) migrate additively under their top-level main
/// once quiescent; an active one is deferred and never re-owned mid-run;
/// nothing nests further.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_nested_children_migrate_without_data_loss() {
    let f = fixture();
    let (core, c) = boot(&f);
    let service = core.rpc_service();
    let mut ev = Evidence::new("legacy-nested-migration");
    let path = project(&c, &core, &f, "chat-model").await;
    let main = main_chat(
        &c,
        "chat-model",
        &path,
        "Legacy main",
        config("fixture", "ask"),
    )
    .await;
    let seed = |parent: String, prompt: &str, key: &str, reports: bool| {
        let service = service.clone();
        let request = serde_json::from_value::<luna_proto::CreateChildThreadRequest>(json!({
            "prompt":prompt,"idempotencyKey":key,"reportResults":reports,"continueParent":false,
            "title":key}))
        .unwrap();
        async move {
            service
                .seed_child_for_qa(parent, request)
                .await
                .unwrap()
                .task_id
        }
    };
    let s1 = seed(main.clone(), "LEGACY_S1", "legacy-s1", false).await;
    let s2 = seed(main.clone(), "LEGACY_S2", "legacy-s2", false).await;
    wait_for("legacy level-one runs", 15, || {
        runs_with(&f, "LEGACY_S1") == 1 && runs_with(&f, "LEGACY_S2") == 1
    })
    .await;
    wait_settled(&core, &s1).await;
    wait_settled(&core, &s2).await;
    let idle = seed(s1.clone(), "LEGACY_IDLE_CHILD", "legacy-idle", true).await;
    let active = seed(
        s2.clone(),
        "HOLD_LEGACY_ACTIVE_CHILD",
        "legacy-active",
        false,
    )
    .await;
    let rows = chats(&c).await;
    let before = json!([
        legacy_evidence(&core, &rows, &idle),
        legacy_evidence(&core, &rows, &active)
    ]);
    let layouts_before = json!([
        ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await["root"],
        ok(&c, "GetChatWorkbench", json!({"mainChatId":s1})).await["root"],
    ]);
    let first = identity(&c, &idle).await;
    ev.step(
        "legacy nesting is recognized from durable lineage",
        json!({"role":"side","mainParentId":main,"legacyNested":true}),
        json!({"role":first["role"],"mainParentId":first["mainParentId"],"legacyNested":first["legacyNested"]}),
        Some(&first),
    );

    // The idle record migrates once its run finished and its result was
    // delivered to its legacy parent.
    wait_for("idle child delivered", 30, || {
        runs_with(&f, "LEGACY_IDLE_CHILD") == 1
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        if identity(&c, &idle).await["displayContainerId"] == main {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "idle legacy child never migrated"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    wait_for("active child running", 15, || {
        core.sessions.turn_in_flight(&active)
    })
    .await;
    // Several sweeps pass while the active record stays deferred.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let deferred = identity(&c, &active).await;
    let rows = chats(&c).await;
    ev.step(
        "the active record is deferred and keeps its owner mid-run",
        json!({"display":s2,"mainParentId":main,"parent":s2,"inFlight":true}),
        json!({"display":deferred["displayContainerId"],"mainParentId":deferred["mainParentId"],
            "parent":row(&rows, &active).pointer("/childThread/parentTaskId"),
            "inFlight":core.sessions.turn_in_flight(&active)}),
        Some(&deferred),
    );
    interrupt(&c, &active).await;
    wait_settled(&core, &active).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        if identity(&c, &active).await["displayContainerId"] == main {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "active legacy child never migrated"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let rows = chats(&c).await;
    let after = json!([
        legacy_evidence(&core, &rows, &idle),
        legacy_evidence(&core, &rows, &active)
    ]);
    ev.unchanged(
        "ids, settings, permissions, routing and lineage preserved",
        &before,
        &after,
    );
    ev.unchanged(
        "saved workbenches preserved",
        &layouts_before,
        &json!([
            ok(&c, "GetChatWorkbench", json!({"mainChatId":main})).await["root"],
            ok(&c, "GetChatWorkbench", json!({"mainChatId":s1})).await["root"],
        ]),
    );
    let migrated = identity(&c, &idle).await;
    let delivered = delivered_outcomes(&rows, &idle);
    ev.step(
        "migrated: main container, legacy parent kept as creator, delivery receipts kept",
        json!({"display":main,"legacyNested":true,"creator":{"taskId":s1,"relation":"createdBy"},
            "delivered":true,"sends":1}),
        json!({"display":migrated["displayContainerId"],"legacyNested":migrated["legacyNested"],
            "creator":migrated["creator"],"delivered":delivered.as_array().is_some_and(|ids| !ids.is_empty()),
            "sends":runs_with(&f, "LEGACY_IDLE_CHILD")}),
        Some(&migrated),
    );
    let nested_tab = refused(
        &c,
        "CreateChildTab",
        json!({"parentTaskId":idle,"idempotencyKey":"deeper"}),
    )
    .await;
    let level_one_tab = refused(
        &c,
        "CreateChildTab",
        json!({"parentTaskId":s1,"idempotencyKey":"deeper-s1"}),
    )
    .await;
    ev.step(
        "no further nesting under migrated or legacy parents",
        json!([{"code":"invalidRole","alternatives":true}, {"code":"invalidRole","alternatives":true}]),
        json!([refusal(&nested_tab), refusal(&level_one_tab)]),
        None,
    );
    ev.write();
    drop(service);
    stop(core, c).await;
}
