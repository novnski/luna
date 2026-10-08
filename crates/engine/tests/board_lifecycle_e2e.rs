//! PB-1 board lifecycle E2E (A2): the real engine over the in-memory RPC
//! transport → the `Board*` dispatcher → lifecycle (intent, generation, stop
//! barrier, admission) → the ordinary task-command path → a deterministic
//! protocol peer, plus restart. Contract:
//! docs/design/project-board/pb0-contract.md §5.4, §7, §11, §12;
//! docs/pending-tasks/devin-project-board/board-lifecycle.md.
//!
//! Failure cases, written before the lifecycle implementation:
//!
//! 1. The first Backlog → Ready move creates zero or two working chats,
//!    sends the first instruction zero or two times, sends it before the
//!    intent and launch snapshot are durable, or sends the brief as a
//!    privileged system message instead of an ordinary task command with
//!    trusted workflow metadata kept apart from the quoted brief.
//! 2. Ready → Backlog uses a board-only kill instead of the chat's own Stop
//!    command (`ExecuteTaskCommand` session interrupt), stops unrelated
//!    sibling chats, archives/erases the chat or its history, or reports
//!    `Stopped` before the engine observed the run end (unknown termination
//!    must stay visible as Stopping/Unknown).
//! 3. Moving the ticket back to Ready after work started creates another chat
//!    or replays the first-start brief; it must continue the same chat with a
//!    continuation instruction (last phase, stop reason, brief delta,
//!    remaining criteria, reviewer state) and keep the first-prompt count 1.
//! 4. Rapid Ready → Backlog → Ready → Backlog → Ready while the old run is
//!    still stopping overlaps two writers, sends more than one continuation,
//!    or loses the latest intent; Ready must show Waiting for stop and send
//!    exactly one continuation after verified quiescence.
//! 5. Two clients repeating the same move request id get different receipts,
//!    or create two chats/prompts; two clients with different request ids
//!    for the same drag also produce exactly one launch.
//! 6. A launch queued by admission (capacity/checkout/dependency) still runs
//!    after the user moves the ticket back to Backlog (must be cancelled
//!    before sending, `CancelledUnsent`).
//! 7. An uncertain first delivery (provider acceptance lost) is retried
//!    blindly by a later move or a restart; it must stay Unknown with
//!    Needs attention (DeliveryUnknown) and be reconciled by the original
//!    command/message id; only an explicit Retry may send again.
//! 8. Restart with a Ready ticket blindly resends the start prompt; restart
//!    with a Stopping ticket claims Stopped without runtime evidence or
//!    launches a second worker.
//! 9. A late Stop acknowledgement or a late passing review for an older
//!    generation/intent revision restarts work, stops the new generation or
//!    marks the ticket Done.
//! 10. Reordering within a column (Ready → Ready) sends, stops, resends or
//!     reviews anything, or changes the revision/intent.
//! 11. An unavailable requested model/effort silently falls back to another
//!     model; it must be refused with the supported choices and the
//!     requested settings preserved (no chat, no prompt).
//! 12. A three-ticket batch from one main chat starts more writers than the
//!     admission bounds allow (one writer per checkout), returns one blanket
//!     result instead of per-ticket Started/Queued/Reused/Failed, or a
//!     retried batch starts twice.
//! 13. A chat that is not the ticket's working chat can report status for
//!     it; a stale generation overwrites the current status; an identical
//!     report adds an event; a report forces Done or claims Stopped.
//! 14. Editing the brief during a run changes the sent snapshot or the
//!     original message; "Send update to chat" must send the new revision
//!     once through the ordinary queue path as a `BriefUpdate` launch.
//! 15. "Start it and report back here" delivers the outcome twice (e.g.
//!     again after restart), delivers it to the wrong chat, or wakes the
//!     source agent when only a report was requested.
//! 16. Settings the user deliberately deferred are silently filled with
//!     defaults (or broader access) at dispatch; Ready must hold the ticket
//!     with a visible SettingsUnresolved reason until they are chosen.
//!
//! Scenarios that create workers depend on the chat-model slice (A3:
//! `board_create_main_chat` / `board_create_side_chat` with a ticket
//! binding) and, for reviewer Stop, on the review slice (A5 owned work);
//! they are written against the contracted behaviour and run by the
//! integrator on the merged tree.
//!
//! Every scenario writes a JSON manifest (steps, expected/observed, SHA-256
//! of snapshots) under `target/qa/board/lifecycle/`.
use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, EngineRpc, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, BoardError, DoneStatus, HarnessId, Model, ModelOption, ModelOptionChoice,
    ReasoningLevel, RunRequest,
};
use luna_rpc::{RpcClient, RpcError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(25);
const BRIEF: &str = "Search must respect the Include archived toggle.\n\n## Acceptance criteria\n- Archived projects are hidden when the toggle is off\n- Current behavior is kept when it is on\n";
const BRIEF_V2: &str = "Search must respect the Include archived toggle.\n\n## Acceptance criteria\n- Archived projects are hidden when the toggle is off\n- Current behavior is kept when it is on\n- The toggle state survives a restart\n";

// ---------------------------------------------------------------------------
// Deterministic protocol peer
// ---------------------------------------------------------------------------

/// What the peer saw and how it behaves per ticket key (parsed from the
/// trusted instruction header). Runs of a held key stay live until released
/// or interrupted; a stubborn key ignores the interrupt (the engine's own
/// grace still ends it); a lost key fails before acknowledging acceptance.
#[derive(Default)]
struct PeerState {
    prompts: Vec<String>,
    hold: HashSet<String>,
    stubborn: HashSet<String>,
    lose: HashSet<String>,
    interrupted: Vec<String>,
}

#[derive(Clone, Default)]
struct PeerHandle(Arc<Mutex<PeerState>>);

impl PeerHandle {
    fn with<T>(&self, f: impl FnOnce(&mut PeerState) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
    fn hold(&self, key: &str) {
        self.with(|s| s.hold.insert(key.into()));
    }
    fn release(&self, key: &str) {
        self.with(|s| s.hold.remove(key));
    }
    fn stubborn(&self, key: &str, on: bool) {
        self.with(|s| {
            if on {
                s.stubborn.insert(key.into())
            } else {
                s.stubborn.remove(key)
            }
        });
    }
    fn lose(&self, key: &str, on: bool) {
        self.with(|s| {
            if on {
                s.lose.insert(key.into())
            } else {
                s.lose.remove(key)
            }
        });
    }
    fn prompts(&self) -> Vec<String> {
        self.with(|s| s.prompts.clone())
    }
    fn for_key(&self, key: &str) -> Vec<String> {
        self.prompts()
            .into_iter()
            .filter(|prompt| ticket_key(prompt).as_deref() == Some(key))
            .collect()
    }
    /// Prompts of one instruction kind (`first start`, `continuation`, …).
    fn count(&self, key: &str, kind: &str) -> usize {
        let header = format!("[LUNA board workflow · {key} · {kind} ·");
        self.prompts()
            .iter()
            .filter(|prompt| prompt.starts_with(&header))
            .count()
    }
    fn interrupted(&self) -> Vec<String> {
        self.with(|s| s.interrupted.clone())
    }
}

fn ticket_key(prompt: &str) -> Option<String> {
    let rest = prompt.strip_prefix("[LUNA board workflow · ")?;
    rest.split(" · ").next().map(str::to_owned)
}

fn model(id: &str, levels: Vec<ReasoningLevel>, options: Vec<ModelOption>) -> Model {
    Model {
        id: id.into(),
        label: id.into(),
        description: None,
        reasoning_levels: levels,
        options,
        first_seen_at_ms: None,
    }
}

struct Peer(PeerHandle);

#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Board lifecycle protocol peer"
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
        Ok(vec![
            model(
                "fixture",
                vec![ReasoningLevel::Low, ReasoningLevel::High],
                vec![ModelOption {
                    id: "speed".into(),
                    label: "Speed".into(),
                    choices: vec![
                        ModelOptionChoice {
                            id: "normal".into(),
                            label: "Normal".into(),
                        },
                        ModelOptionChoice {
                            id: "fast".into(),
                            label: "Fast".into(),
                        },
                    ],
                    default_choice: "normal".into(),
                }],
            ),
            model("fixture-pro", vec![ReasoningLevel::High], vec![]),
        ])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let key = ticket_key(&request.prompt).unwrap_or_else(|| "chat".into());
        let lost = self.0.with(|s| {
            s.prompts.push(request.prompt.clone());
            s.lose.contains(&key)
        });
        if lost {
            return Err(HarnessError::Protocol("acceptance response lost".into()));
        }
        let state = self.0.clone();
        let session = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("native-{}", uuid::Uuid::new_v4()));
        let done_session = session.clone();
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: request.model.clone().unwrap_or_else(|| "fixture".into()),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: session,
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("Working on {key}"),
            }),
        ])
        .chain(futures::stream::once(async move {
            let status = loop {
                let outcome = state.with(|s| {
                    if !s.hold.contains(&key) {
                        Some(DoneStatus::Completed)
                    } else if controls.interrupt.is_cancelled() && !s.stubborn.contains(&key) {
                        s.interrupted.push(key.clone());
                        Some(DoneStatus::Interrupted)
                    } else {
                        None
                    }
                });
                if let Some(status) = outcome {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            Ok(AgentEvent::Done {
                status,
                result: None,
                error: None,
                session_id: Some(done_session),
            })
        }))
        .boxed())
    }
}

// ---------------------------------------------------------------------------
// Engine fixture
// ---------------------------------------------------------------------------

struct Fixture {
    _parent: tempfile::TempDir,
    root: PathBuf,
    projects: PathBuf,
    peer: PeerHandle,
}

fn fixture() -> Fixture {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("synthetic-luna");
    let projects = parent.path().join("projects");
    for dir in [&root, &projects] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        root.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
    Fixture {
        _parent: parent,
        root,
        projects,
        peer: PeerHandle::default(),
    }
}

struct Boot {
    core: EngineCore,
    rpc: Arc<EngineRpc>,
    c: RpcClient,
}

fn boot(f: &Fixture) -> Boot {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer(f.peer.clone())));
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
    let rpc = core.rpc_service();
    let c = luna_rpc::memory_client(rpc.clone());
    Boot { core, rpc, c }
}

async fn stop(b: Boot) {
    b.core.shutdown().await;
    drop(b.c);
    drop(b.rpc);
    drop(b.core);
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

fn hook_error(result: Result<Value, String>) -> BoardError {
    let message = result.expect_err("the call unexpectedly succeeded");
    BoardError::from_rpc_message(&message).unwrap_or_else(|| panic!("not a board error: {message}"))
}

async fn space(b: &Boot, f: &Fixture, id: &str, folder: &str) -> String {
    let path = f.projects.join(folder);
    std::fs::create_dir_all(&path).unwrap();
    let path = path.to_string_lossy().to_string();
    ok(
        &b.c,
        "Mutate",
        json!({"op":"createSpace","spaceId":id,"deviceId":b.core.device_id,"path":path}),
    )
    .await;
    path
}

/// An ordinary main chat (the source discussion or a requesting main).
async fn main_chat(b: &Boot, project: &str, task_id: &str, cwd: &str) {
    ok(&b.c, "ExecuteTaskCommand", json!({"commandId":format!("create-{task_id}"),"taskId":task_id,"expectedTaskRevision":0,"intent":{"kind":"createChat","spaceId":project,"cwd":cwd,"config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask"}}})).await;
    tokio::time::timeout(WAIT, async {
        while b.core.workspace.chat(task_id).unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the chat row never appeared");
}

/// A task command with the current fences (as the desktop client sends).
async fn task_command(b: &Boot, task_id: &str, intent: Value) -> Value {
    let snapshot = ok(&b.c, "GetTaskCommandSnapshot", json!({"taskId":task_id})).await;
    ok(&b.c, "ExecuteTaskCommand", json!({"taskId":task_id,"commandId":uuid::Uuid::new_v4().to_string(),"actorId":"qa","clientId":"qa","clientSessionId":"qa","expectedTaskRevision":snapshot["snapshot"]["taskRevision"],"expectedBindingGeneration":snapshot["snapshot"]["bindingGeneration"],"intent":intent})).await
}

/// Save a ticket (Backlog, no chat) and return (id, key).
async fn ticket(b: &Boot, request_id: &str, project: &str, title: &str) -> (String, String) {
    let receipt = ok(
        &b.c,
        "BoardCreate",
        json!({"requestId":request_id,"projectId":project,"title":title,"description":BRIEF}),
    )
    .await;
    (
        receipt["workItemId"].as_str().unwrap().to_owned(),
        receipt["key"].as_str().unwrap().to_owned(),
    )
}

async fn mv(b: &Boot, request_id: &str, id: &str, to: &str) -> Value {
    ok(
        &b.c,
        "BoardMove",
        json!({"requestId":request_id,"workItemId":id,"to":to}),
    )
    .await
}

async fn read(b: &Boot, id: &str) -> Value {
    ok(
        &b.c,
        "BoardRead",
        json!({"workItemId":id,"historyLimit":100}),
    )
    .await
}

fn phase(detail: &Value) -> String {
    detail["view"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn attention_kinds(detail: &Value) -> Vec<String> {
    detail["view"]["attention"]
        .as_array()
        .map(|reasons| {
            reasons
                .iter()
                .filter_map(|reason| reason["kind"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Poll the ticket (driving the production sweep) until `done` holds.
async fn wait_for(b: &Boot, id: &str, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        b.rpc.board_lifecycle_tick().await.unwrap();
        let detail = read(b, id).await;
        if done(&detail) {
            return detail;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}: phase={} intent={}",
                phase(&detail),
                detail["intent"]
            );
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn wait_phase(b: &Boot, id: &str, wanted: &str) -> Value {
    wait_for(b, id, wanted, |detail| phase(detail) == wanted).await
}

fn worker(detail: &Value) -> Option<String> {
    detail["links"].as_array()?.iter().find_map(|link| {
        (link["role"] == "working" && link["active"] == true)
            .then(|| link["taskId"].as_str().unwrap().to_owned())
    })
}

fn launches(detail: &Value) -> Vec<Value> {
    detail["launches"].as_array().cloned().unwrap_or_default()
}

async fn chat_rows(b: &Boot) -> Vec<Value> {
    let mut watch = b.c.subscribe("WatchChats", json!({})).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap();
    first.as_array().cloned().unwrap_or_default()
}

fn transcript(b: &Boot, task_id: &str) -> Vec<luna_doc::SessionMessageEntry> {
    b.core
        .doc_host
        .open(task_id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
}

fn entry_text(entry: &luna_doc::SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            luna_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn event_kinds(b: &Boot, id: &str) -> Vec<String> {
    ok(&b.c, "BoardEvents", json!({"workItemId":id,"limit":500})).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"]["kind"].as_str().unwrap().to_owned())
        .collect()
}

fn count(kinds: &[String], kind: &str) -> usize {
    kinds.iter().filter(|candidate| *candidate == kind).count()
}

fn sha256(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn sha256_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
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
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/lifecycle");
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
            .unwrap_or_else(|| json!({"suite":"board_lifecycle_e2e","scenarios":{}}));
        manifest["suite"] = json!("board_lifecycle_e2e");
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

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Cases 1, 2, 3, 10: first move → one chat + one prompt; Backlog → the
/// chat's own Stop with history kept; Ready again → the same chat with a
/// continuation; reorders have no execution effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_start_stop_and_continue_share_one_chat_and_one_brief() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("first-start-stop-continue");
    space(&b, &f, "project-orbit", "orbit").await;
    let (id, key) = ticket(
        &b,
        "ticket-a",
        "project-orbit",
        "Respect the archived toggle",
    )
    .await;
    let (other, _) = ticket(&b, "ticket-b", "project-orbit", "Keep the toggle sticky").await;
    f.peer.hold(&key);
    let chats_before = chat_rows(&b).await.len();

    let rank = ok(
        &b.c,
        "BoardMove",
        json!({"requestId":"rank-a","workItemId":id,"to":"backlog","afterId":other}),
    )
    .await;
    ev.step(
        "reorder within Backlog has no execution effect",
        json!({"effect":"none","prompts":0,"revision":1,"intentRevision":0}),
        json!({"effect":rank["effect"]["kind"],"prompts":f.peer.prompts().len(),"revision":rank["mutation"]["revision"],"intentRevision":rank["intent"]["intentRevision"]}),
        Some(&rank),
    );

    let first = mv(&b, "to-ready-1", &id, "ready").await;
    let working = wait_phase(&b, &id, "working").await;
    let task = worker(&working).expect("a working chat");
    let first_launch = launches(&working)[0].clone();
    let instruction = first_launch["instructionText"].as_str().unwrap().to_owned();
    ev.step(
        "first move: one working chat, one first-start instruction",
        json!({"effect":"firstStart","chats":chats_before + 1,"firstStarts":1,"launches":1,"kind":"firstStart","firstPrompt":"delivered","generation":1}),
        json!({"effect":first["effect"]["kind"],"chats":chat_rows(&b).await.len(),"firstStarts":f.peer.count(&key,"first start"),"launches":launches(&working).len(),"kind":first_launch["kind"],"firstPrompt":working["intent"]["firstPrompt"]["kind"],"generation":working["intent"]["executionGeneration"]}),
        Some(&working),
    );
    ev.step(
        "the instruction is an ordinary user turn: trusted header, quoted brief, frozen hash",
        json!({"header":true,"quoted":true,"metadata":true,"hash":true,"sentAsUserTurn":true,"providerSawExactText":true}),
        json!({
            "header": instruction.starts_with(&format!("[LUNA board workflow · {key} · first start · brief revision 1 · generation 1]")),
            "quoted": instruction.contains(&format!("<ticket-brief key=\"{key}\" revision=\"1\">")),
            "metadata": instruction.contains("Workflow metadata (from LUNA, trusted):"),
            "hash": first_launch["instructionHash"] == json!(sha256_text(&instruction)),
            "sentAsUserTurn": transcript(&b, &task).iter().any(|entry| entry.id == first_launch["messageId"].as_str().unwrap() && entry.role == luna_doc::MessageRole::User),
            "providerSawExactText": f.peer.for_key(&key).first() == Some(&instruction),
        }),
        Some(&first_launch),
    );

    let again = mv(&b, "ready-again", &id, "ready").await;
    ev.step(
        "Ready → Ready causes no provider traffic",
        json!({"effect":"coalesced","prompts":1}),
        json!({"effect":again["effect"]["kind"],"prompts":f.peer.for_key(&key).len()}),
        Some(&again),
    );

    let hold = mv(&b, "to-backlog", &id, "backlog").await;
    let stopped = wait_phase(&b, &id, "stopped").await;
    let stop_command = stopped["intent"]["stop"]["stopReceiptIds"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let stop_receipt = ok(
        &b.c,
        "GetTaskCommandReceipt",
        json!({"commandId":stop_command}),
    )
    .await;
    let row = chat_rows(&b)
        .await
        .into_iter()
        .find(|chat| chat["id"] == task.as_str())
        .expect("the working chat still exists");
    ev.step(
        "Backlog sends the chat's own Stop to the exact worker and keeps the chat",
        json!({"effect":"stopRequested","generation":1,"intent":"stop","task":task,"interrupted":[key.clone()],"archived":false,"historyKept":true,"barrier":"stopped"}),
        json!({
            "effect": hold["effect"]["kind"],
            "generation": hold["effect"]["executionGeneration"],
            "intent": stop_receipt["intent"],
            "task": stop_receipt["taskId"],
            "interrupted": f.peer.interrupted(),
            "archived": row["archived"],
            "historyKept": transcript(&b, &task).iter().any(|entry| entry.id == first_launch["messageId"].as_str().unwrap()),
            "barrier": stopped["intent"]["stop"]["state"],
        }),
        Some(&stopped),
    );

    let revision = stopped["view"]["item"]["revision"].clone();
    ok(
        &b.c,
        "BoardUpdate",
        json!({"requestId":"edit-brief","workItemId":id,"expectedRevision":revision,"changes":{"description":BRIEF_V2}}),
    )
    .await;
    let resume = mv(&b, "to-ready-2", &id, "ready").await;
    let resumed = wait_for(&b, &id, "the continuation run", |detail| {
        phase(detail) == "working" && detail["intent"]["executionGeneration"] == 2
    })
    .await;
    let continuation = f
        .peer
        .for_key(&key)
        .into_iter()
        .find(|prompt| prompt.contains("· continuation ·"))
        .expect("a continuation prompt");
    ev.step(
        "Ready again continues the same chat; the first prompt is never replayed",
        json!({"effect":"continue","sameChat":true,"chats":chats_before + 1,"firstStarts":1,"continuations":1}),
        json!({"effect":resume["effect"]["kind"],"sameChat":worker(&resumed) == Some(task.clone()),"chats":chat_rows(&b).await.len(),"firstStarts":f.peer.count(&key,"first start"),"continuations":f.peer.count(&key,"continuation")}),
        Some(&resumed),
    );
    ev.step(
        "the continuation reports phase, stop reason, brief delta, criteria and review state",
        json!({"phase":true,"stop":true,"delta":true,"criteria":true,"review":true,"newBrief":true}),
        json!({
            "phase": continuation.contains("- Last confirmed phase:"),
            "stop": continuation.contains("- Stop reason: the previous run (generation 1) was stopped by the user"),
            "delta": continuation.contains("changed from revision 1 to 2"),
            "criteria": continuation.contains("The toggle state survives a restart"),
            "review": continuation.contains("- Reviewer state:"),
            "newBrief": continuation.contains(&format!("<ticket-brief key=\"{key}\" revision=\"2\">")),
        }),
        None,
    );
    f.peer.release(&key);
    let idle = wait_phase(&b, &id, "idle").await;
    ev.step(
        "the finished run leaves the ticket Ready and idle (Done needs review evidence)",
        json!({"column":"ready","launchState":"delivered"}),
        json!({"column":idle["view"]["item"]["column"],"launchState":idle["intent"]["launchState"]}),
        Some(&idle),
    );
    ev.write();
    stop(b).await;
}

/// Case 4: rapid toggles behind a slow Stop coalesce into one continuation
/// sent only after the old run ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_toggles_wait_for_stop_and_send_one_continuation() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("rapid-toggles-stop-barrier");
    space(&b, &f, "project-orbit", "orbit").await;
    let (id, key) = ticket(&b, "ticket-a", "project-orbit", "Debounce the search box").await;
    f.peer.hold(&key);
    f.peer.stubborn(&key, true);
    mv(&b, "r1", &id, "ready").await;
    let working = wait_phase(&b, &id, "working").await;
    let task = worker(&working).unwrap();
    let hold = mv(&b, "b1", &id, "backlog").await;
    let ready = mv(&b, "r2", &id, "ready").await;
    let held_again = mv(&b, "b2", &id, "backlog").await;
    let ready_again = mv(&b, "r3", &id, "ready").await;
    let waiting = read(&b, &id).await;
    ev.step(
        "toggles while the old run is stopping wait behind the barrier",
        json!({"stop":"stopRequested","ready":"queued","readyReason":"waitingForStop","hold":"cancelledUnsent","readyAgain":"queued","phase":"waitingForStop","prompts":1,"liveRun":true}),
        json!({
            "stop": hold["effect"]["kind"],
            "ready": ready["effect"]["kind"],
            "readyReason": ready["effect"]["reason"]["kind"],
            "hold": held_again["effect"]["kind"],
            "readyAgain": ready_again["effect"]["kind"],
            "phase": phase(&waiting),
            "prompts": f.peer.for_key(&key).len(),
            "liveRun": b.core.sessions.turn_in_flight(&task),
        }),
        Some(&waiting),
    );
    // The provider ignores the interrupt; the engine's own grace ends the
    // run, and only then may the continuation be sent.
    let resumed = wait_for(&b, &id, "the single continuation", |detail| {
        detail["intent"]["executionGeneration"] == 2 && phase(detail) == "working"
    })
    .await;
    let kinds = event_kinds(&b, &id).await;
    ev.step(
        "exactly one continuation after verified quiescence, same chat",
        json!({"firstStarts":1,"continuations":1,"launches":2,"intentRevision":5,"sameChat":true,"stopRequested":1,"stopSettled":1}),
        json!({
            "firstStarts": f.peer.count(&key, "first start"),
            "continuations": f.peer.count(&key, "continuation"),
            "launches": launches(&resumed).len(),
            "intentRevision": resumed["intent"]["intentRevision"],
            "sameChat": worker(&resumed) == Some(task.clone()),
            "stopRequested": count(&kinds, "stopRequested"),
            "stopSettled": count(&kinds, "stopSettled"),
        }),
        Some(&resumed),
    );
    f.peer.stubborn(&key, false);
    f.peer.release(&key);
    ev.write();
    stop(b).await;
}

/// Case 5: two clients repeating one request id share its receipt; two
/// clients with their own ids for the same drag still produce one launch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_requests_from_two_clients_share_one_outcome() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("two-clients-one-outcome");
    space(&b, &f, "project-orbit", "orbit").await;
    let (a, key_a) = ticket(&b, "ticket-a", "project-orbit", "Two windows, one request").await;
    let (c, key_c) = ticket(&b, "ticket-c", "project-orbit", "Two windows, two requests").await;
    f.peer.hold(&key_a);
    f.peer.hold(&key_c);
    let other = luna_rpc::memory_client(b.rpc.clone());
    let request = json!({"requestId":"drag-1","workItemId":a,"to":"ready"});
    let (one, two) = tokio::join!(
        ok(&b.c, "BoardMove", request.clone()),
        ok(&other, "BoardMove", request.clone())
    );
    wait_phase(&b, &a, "working").await;
    ev.step(
        "the same request id from two clients returns one identical receipt",
        json!({"identical":true,"firstStarts":1,"launches":1}),
        json!({"identical":one == two,"firstStarts":f.peer.count(&key_a,"first start"),"launches":launches(&read(&b,&a).await).len()}),
        Some(&one),
    );
    let (left, right) = tokio::join!(
        ok(
            &b.c,
            "BoardMove",
            json!({"requestId":"window-left","workItemId":c,"to":"ready"})
        ),
        ok(
            &other,
            "BoardMove",
            json!({"requestId":"window-right","workItemId":c,"to":"ready"})
        )
    );
    wait_phase(&b, &c, "queued").await;
    let mut effects = vec![
        left["effect"]["kind"].as_str().unwrap().to_owned(),
        right["effect"]["kind"].as_str().unwrap().to_owned(),
    ];
    effects.sort();
    ev.step(
        "different request ids for one drag: one accepted, one coalesced, one launch at most",
        json!({"effects":["coalesced","queued"],"prompts":0}),
        json!({"effects":effects,"prompts":f.peer.for_key(&key_c).len()}),
        None,
    );
    f.peer.release(&key_a);
    wait_for(&b, &c, "the queued ticket to start", |detail| {
        phase(detail) == "working"
    })
    .await;
    ev.step(
        "the second ticket starts once the checkout is free, exactly once",
        json!({"firstStarts":1}),
        json!({"firstStarts":f.peer.count(&key_c,"first start")}),
        None,
    );
    f.peer.release(&key_c);
    ev.write();
    stop(b).await;
}

/// Case 6: a launch queued by admission is cancelled before sending when
/// the ticket returns to Backlog.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_launch_is_cancelled_before_sending() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("queued-cancelled-before-send");
    space(&b, &f, "project-orbit", "orbit").await;
    let (a, key_a) = ticket(&b, "ticket-a", "project-orbit", "Owns the checkout").await;
    let (q, key_q) = ticket(&b, "ticket-q", "project-orbit", "Waits for the checkout").await;
    f.peer.hold(&key_a);
    f.peer.hold(&key_q);
    mv(&b, "a-ready", &a, "ready").await;
    let running = wait_phase(&b, &a, "working").await;
    let owner = worker(&running).unwrap();
    let queued = mv(&b, "q-ready", &q, "ready").await;
    let shown = read(&b, &q).await;
    ev.step(
        "the second writer in one checkout is queued with a visible reason",
        json!({"effect":"queued","reason":"checkoutBusy","owner":owner,"column":"ready","phase":"queued","chat":null}),
        json!({"effect":queued["effect"]["kind"],"reason":shown["view"]["queueReason"]["kind"],"owner":shown["view"]["queueReason"]["taskId"],"column":shown["view"]["item"]["column"],"phase":phase(&shown),"chat":worker(&shown)}),
        Some(&shown),
    );
    let cancelled = mv(&b, "q-backlog", &q, "backlog").await;
    f.peer.release(&key_a);
    wait_phase(&b, &a, "idle").await;
    for _ in 0..5 {
        b.rpc.board_lifecycle_tick().await.unwrap();
    }
    let after = read(&b, &q).await;
    ev.step(
        "Backlog cancels the unsent launch; it never runs later",
        json!({"effect":"cancelledUnsent","prompts":0,"chat":null,"launches":0,"phase":"notStarted"}),
        json!({"effect":cancelled["effect"]["kind"],"prompts":f.peer.for_key(&key_q).len(),"chat":worker(&after),"launches":launches(&after).len(),"phase":phase(&after)}),
        Some(&after),
    );
    ev.write();
    stop(b).await;
}

/// Cases 7 and 8: an unprovable first delivery stays Unknown, is never
/// resent by moves or a restart, and only an explicit Retry sends again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_first_delivery_is_reconciled_not_resent() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("unknown-first-delivery");
    space(&b, &f, "project-orbit", "orbit").await;
    let (id, key) = ticket(&b, "ticket-a", "project-orbit", "Lost acknowledgement").await;
    f.peer.lose(&key, true);
    mv(&b, "ready-1", &id, "ready").await;
    let unknown = wait_phase(&b, &id, "unknown").await;
    ev.step(
        "lost acceptance is Unknown with Needs attention, not delivered or failed",
        json!({"firstPrompt":"unknown","launchState":"unknown","attention":true,"prompts":1}),
        json!({"firstPrompt":unknown["intent"]["firstPrompt"]["kind"],"launchState":unknown["intent"]["launchState"],"attention":attention_kinds(&unknown).contains(&"deliveryUnknown".to_owned()),"prompts":f.peer.for_key(&key).len()}),
        Some(&unknown),
    );
    mv(&b, "backlog-1", &id, "backlog").await;
    let again = mv(&b, "ready-2", &id, "ready").await;
    ev.step(
        "moving again reconciles instead of resending",
        json!({"effect":"reconciling","prompts":1}),
        json!({"effect":again["effect"]["kind"],"prompts":f.peer.for_key(&key).len()}),
        Some(&again),
    );
    stop(b).await;
    let b = boot(&f);
    let report = b.rpc.board_qa_reconcile().await.unwrap();
    for _ in 0..3 {
        b.rpc.board_lifecycle_tick().await.unwrap();
    }
    let after = read(&b, &id).await;
    ev.step(
        "restart restores the intent and still does not resend",
        json!({"prompts":1,"leftUnknown":true,"firstPrompt":"unknown","phase":"unknown"}),
        json!({"prompts":f.peer.for_key(&key).len(),"leftUnknown":report["leftUnknown"].as_array().unwrap().iter().any(|v| v == id.as_str()),"firstPrompt":after["intent"]["firstPrompt"]["kind"],"phase":phase(&after)}),
        Some(&after),
    );
    f.peer.lose(&key, false);
    let retry = ok(
        &b.c,
        "BoardControl",
        json!({"requestId":"retry-1","workItemId":id,"action":"retry"}),
    )
    .await;
    let retried = wait_phase(&b, &id, "idle").await;
    ev.step(
        "only an explicit Retry sends again, once, as a retry",
        json!({"effect":"continue","retries":1,"firstStarts":1,"prompts":2,"firstPrompt":"delivered"}),
        json!({"effect":retry["effect"]["kind"],"retries":f.peer.count(&key,"retry"),"firstStarts":f.peer.count(&key,"first start"),"prompts":f.peer.for_key(&key).len(),"firstPrompt":retried["intent"]["firstPrompt"]["kind"]}),
        Some(&retried),
    );
    ev.write();
    stop(b).await;
}

/// Case 8: restart with a Ready ticket never resends its start; restart
/// with a Stopping ticket settles from runtime evidence, never a relaunch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_with_ready_and_stopping_never_resends() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("restart-ready-and-stopping");
    space(&b, &f, "project-orbit", "orbit").await;
    space(&b, &f, "project-lunar", "lunar").await;
    let (ready, key_ready) = ticket(&b, "ticket-ready", "project-orbit", "Keeps running").await;
    let (stopping, key_stopping) =
        ticket(&b, "ticket-stopping", "project-lunar", "Slow to stop").await;
    f.peer.hold(&key_ready);
    f.peer.hold(&key_stopping);
    f.peer.stubborn(&key_stopping, true);
    mv(&b, "ready-1", &ready, "ready").await;
    mv(&b, "stopping-1", &stopping, "ready").await;
    wait_phase(&b, &ready, "working").await;
    let running = wait_phase(&b, &stopping, "working").await;
    let stopping_worker = worker(&running).unwrap();
    let hold = mv(&b, "stopping-2", &stopping, "backlog").await;
    let before = read(&b, &stopping).await;
    ev.step(
        "the stubborn run is still stopping when the engine shuts down",
        json!({"effect":"stopRequested","phase":"stopping"}),
        json!({"effect":hold["effect"]["kind"],"phase":phase(&before)}),
        Some(&before),
    );
    stop(b).await;
    f.peer.stubborn(&key_stopping, false);
    let b = boot(&f);
    let report = b.rpc.board_qa_reconcile().await.unwrap();
    let ready_after = wait_phase(&b, &ready, "idle").await;
    let stopping_after = read(&b, &stopping).await;
    ev.step(
        "restart: Ready is restored without a resend; Stopping settles without a relaunch",
        json!({"readyPrompts":1,"readyColumn":"ready","readyFirstPrompt":"delivered","stoppingPrompts":1,"stoppingSettled":true,"stoppingColumn":"backlog","sameWorker":true,"examined":true}),
        json!({
            "readyPrompts": f.peer.for_key(&key_ready).len(),
            "readyColumn": ready_after["view"]["item"]["column"],
            "readyFirstPrompt": ready_after["intent"]["firstPrompt"]["kind"],
            "stoppingPrompts": f.peer.for_key(&key_stopping).len(),
            "stoppingSettled": (["stopped", "unknown"].contains(&phase(&stopping_after).as_str())),
            "stoppingColumn": stopping_after["view"]["item"]["column"],
            "sameWorker": worker(&stopping_after) == Some(stopping_worker),
            "examined": report["examined"].as_u64().unwrap() >= 2,
        }),
        Some(&stopping_after),
    );
    f.peer.release(&key_ready);
    f.peer.release(&key_stopping);
    ev.write();
    stop(b).await;
}

/// Case 9: late Stop acknowledgements and late passing reviews for an
/// older generation or intent revision change nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_stop_and_review_callbacks_change_nothing() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("stale-callbacks-ignored");
    space(&b, &f, "project-orbit", "orbit").await;
    let (id, key) = ticket(&b, "ticket-a", "project-orbit", "Generations stay apart").await;
    f.peer.hold(&key);
    mv(&b, "r1", &id, "ready").await;
    let gen1 = wait_phase(&b, &id, "working").await;
    let task = worker(&gen1).unwrap();
    let gen1_revision = gen1["intent"]["intentRevision"].as_u64().unwrap();
    mv(&b, "b1", &id, "backlog").await;
    let stopped = wait_phase(&b, &id, "stopped").await;
    let old_stop = stopped["intent"]["stop"]["stopReceiptIds"][0]
        .as_str()
        .unwrap()
        .to_owned();
    mv(&b, "r2", &id, "ready").await;
    let gen2 = wait_for(&b, &id, "generation 2", |detail| {
        detail["intent"]["executionGeneration"] == 2 && phase(detail) == "working"
    })
    .await;
    // A late delivery of generation 1's Stop is its stored receipt only.
    let replayed = ok(&b.c, "ExecuteTaskCommand", json!({"commandId":old_stop,"taskId":task,"actorId":"luna-board","clientId":"luna-board-lifecycle","clientSessionId":"late","intent":{"kind":"session","command":{"kind":"interrupt"}}})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still = read(&b, &id).await;
    ev.step(
        "a late Stop for generation 1 cannot stop generation 2",
        json!({"phase":"working","live":true,"generation":2}),
        json!({"phase":phase(&still),"live":b.core.sessions.turn_in_flight(&task),"generation":still["intent"]["executionGeneration"]}),
        Some(&replayed),
    );
    let late_old = b
        .rpc
        .board_qa_mark_done(&id, 1, gen1_revision)
        .await
        .unwrap();
    let gen2_revision = gen2["intent"]["intentRevision"].as_u64().unwrap();
    mv(&b, "b2", &id, "backlog").await;
    wait_phase(&b, &id, "stopped").await;
    let late_held = b
        .rpc
        .board_qa_mark_done(&id, 2, gen2_revision)
        .await
        .unwrap();
    let held = read(&b, &id).await;
    ev.step(
        "late passing reviews for an older generation or a held ticket change nothing",
        json!({"old":null,"held":null,"column":"backlog","completed":0}),
        json!({"old":late_old,"held":late_held,"column":held["view"]["item"]["column"],"completed":count(&event_kinds(&b,&id).await,"completed")}),
        Some(&held),
    );
    mv(&b, "r3", &id, "ready").await;
    wait_for(&b, &id, "generation 3", |detail| {
        detail["intent"]["executionGeneration"] == 3 && phase(detail) == "working"
    })
    .await;
    f.peer.release(&key);
    let idle = wait_phase(&b, &id, "idle").await;
    let current = b
        .rpc
        .board_qa_mark_done(&id, 3, idle["intent"]["intentRevision"].as_u64().unwrap())
        .await
        .unwrap();
    let done = read(&b, &id).await;
    ev.step(
        "the current generation's passing result reaches Done once",
        json!({"receipt":true,"column":"done","phase":"complete","completed":1}),
        json!({"receipt":current.is_some(),"column":done["view"]["item"]["column"],"phase":phase(&done),"completed":count(&event_kinds(&b,&id).await,"completed")}),
        Some(&done),
    );
    ev.write();
    stop(b).await;
}

/// Case 11: an unavailable model or effort is refused with the supported
/// choices; nothing is created or sent and the saved settings are kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_model_is_refused_without_fallback() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("unavailable-model-no-fallback");
    space(&b, &f, "project-orbit", "orbit").await;
    let saved = ok(
        &b.c,
        "BoardCreate",
        json!({"requestId":"ticket-a","projectId":"project-orbit","title":"Pinned model","description":BRIEF,"implementation":{"harness":"mock","model":"fixture"}}),
    )
    .await;
    let id = saved["workItemId"].as_str().unwrap().to_owned();
    let chats_before = chat_rows(&b).await.len();
    let missing = refused(
        &b.c,
        "BoardMove",
        json!({"requestId":"m1","workItemId":id,"to":"ready","execution":{"model":"missing-model"}}),
    )
    .await;
    let effort = refused(
        &b.c,
        "BoardMove",
        json!({"requestId":"m2","workItemId":id,"to":"ready","execution":{"model":"fixture-pro","reasoning":"low"}}),
    )
    .await;
    let after = read(&b, &id).await;
    let (supported_models, supported_efforts) = match (&missing, &effort) {
        (
            BoardError::UnavailableModel { supported, .. },
            BoardError::UnsupportedEffort {
                supported: efforts, ..
            },
        ) => (json!(supported), json!(efforts)),
        other => panic!("unexpected errors: {other:?}"),
    };
    ev.step(
        "no silent fallback: structured refusals with the supported choices",
        json!({"model":"unavailableModel","models":["fixture","fixture-pro"],"effort":"unsupportedEffort","efforts":["high"]}),
        json!({"model":missing.code(),"models":supported_models,"effort":effort.code(),"efforts":supported_efforts}),
        None,
    );
    ev.step(
        "the refused start leaves no chat, no prompt and the saved settings intact",
        json!({"column":"backlog","chats":chats_before,"prompts":0,"launches":0,"savedModel":"fixture","intentRevision":null}),
        json!({"column":after["view"]["item"]["column"],"chats":chat_rows(&b).await.len(),"prompts":f.peer.prompts().len(),"launches":launches(&after).len(),"savedModel":after["view"]["item"]["implementation"]["model"],"intentRevision":after["intent"]["intentRevision"]}),
        Some(&after),
    );
    let save_time = refused(
        &b.c,
        "BoardUpdate",
        json!({"requestId":"u1","workItemId":id,"expectedRevision":1,"changes":{"implementation":{"harness":"mock","model":"retired-model"}}}),
    )
    .await;
    let preview = ok(&b.c, "BoardResolveSettings", json!({"workItemId":id})).await;
    ev.step(
        "save-time validation uses the cached catalog; preview shows the reviewer fallback",
        json!({"save":"unavailableModel","model":"fixture","reasoning":"high","reviewFallback":true}),
        json!({"save":save_time.code(),"model":preview["implementation"]["model"],"reasoning":preview["implementation"]["reasoning"],"reviewFallback":preview["reviewFallbackToImplementation"]}),
        Some(&preview),
    );
    ev.write();
    stop(b).await;
}

/// Case 12: a three-ticket batch from one main chat creates side chats
/// under it, admits one writer per checkout and reports per ticket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_ticket_batch_is_bounded_with_per_ticket_results() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("batch-bounded-admission");
    let path = space(&b, &f, "project-orbit", "orbit").await;
    let main = "a7777777-7777-4777-8777-777777777777";
    main_chat(&b, "project-orbit", main, &path).await;
    let mut ids = Vec::new();
    let mut keys = Vec::new();
    for (index, title) in ["Batch one", "Batch two", "Batch three"].iter().enumerate() {
        let (id, key) = ticket(&b, &format!("batch-ticket-{index}"), "project-orbit", title).await;
        f.peer.hold(&key);
        ids.push(id);
        keys.push(key);
    }
    let request = json!({"requestId":"batch-1","workItemIds":ids,"callerTaskId":main});
    let batch = ok(&b.c, "BoardStartBatch", request.clone()).await;
    let outcomes: Vec<Value> = batch["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| {
            json!([
                result["outcome"]["kind"],
                result["outcome"]["reason"]["kind"]
            ])
        })
        .collect();
    let first = &batch["results"][0]["receipt"];
    ev.step(
        "per-ticket results: one started side chat, two queued behind the checkout",
        json!({"outcomes":[["started",null],["queued","checkoutBusy"],["queued","checkoutBusy"]],"role":"side","parent":main}),
        json!({"outcomes":outcomes,"role":first["worker"]["role"],"parent":first["worker"]["mainParentId"]}),
        Some(&batch),
    );
    let replay = ok(&b.c, "BoardStartBatch", request).await;
    ev.step(
        "a retried batch returns the same results and starts nothing twice",
        json!({"identical":true,"firstStarts":1}),
        json!({"identical":replay == batch,"firstStarts":keys.iter().map(|key| f.peer.count(key,"first start")).sum::<usize>()}),
        None,
    );
    let mut max_live = 0;
    for index in 0..3 {
        let running = wait_phase(&b, &ids[index], "working").await;
        let mut live = 0;
        for other in &ids {
            if let Some(task) = worker(&read(&b, other).await)
                && b.core.sessions.turn_in_flight(&task)
            {
                live += 1;
            }
        }
        max_live = max_live.max(live);
        assert_eq!(
            running["links"]
                .as_array()
                .unwrap()
                .iter()
                .find(|link| link["role"] == "working")
                .unwrap()["originalContainer"]["mainTaskId"],
            main
        );
        f.peer.release(&keys[index]);
        wait_phase(&b, &ids[index], "idle").await;
    }
    ev.step(
        "the queue drains one writer at a time; each ticket got one first start",
        json!({"maxLive":1,"firstStarts":[1,1,1]}),
        json!({"maxLive":max_live,"firstStarts":keys.iter().map(|key| f.peer.count(key,"first start")).collect::<Vec<_>>()}),
        None,
    );
    ev.write();
    stop(b).await;
}

/// Case 13: only the working chat (or a user-requested manager) reports;
/// stale generations are refused; identical reports are no-ops; reports
/// cannot express Done or Stopped and never override the engine phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_reports_are_owner_bound_and_stale_safe() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("status-owner-bound");
    let path = space(&b, &f, "project-orbit", "orbit").await;
    let bystander = "a8888888-8888-4888-8888-888888888888";
    main_chat(&b, "project-orbit", bystander, &path).await;
    let (id, key) = ticket(&b, "ticket-a", "project-orbit", "Reports its own status").await;
    f.peer.hold(&key);
    mv(&b, "r1", &id, "ready").await;
    let working = wait_phase(&b, &id, "working").await;
    let task = worker(&working).unwrap();
    let report = |request_id: &str, generation: u64, kind: &str| json!({"requestId":request_id,"workItemId":id,"executionGeneration":generation,"report":{"kind":kind,"summary":"Waiting on the schema decision"}});
    let foreign = hook_error(
        b.rpc
            .board_qa_call_as_agent(bystander, "setStatus", report("s0", 1, "blocked"))
            .await,
    );
    let own = b
        .rpc
        .board_qa_call_as_agent(&task, "setStatus", report("s1", 1, "blocked"))
        .await
        .unwrap();
    let repeat = b
        .rpc
        .board_qa_call_as_agent(&task, "setStatus", report("s2", 1, "blocked"))
        .await
        .unwrap();
    let stale = hook_error(
        b.rpc
            .board_qa_call_as_agent(&task, "setStatus", report("s3", 7, "progress"))
            .await,
    );
    let forged = hook_error(
        b.rpc
            .board_qa_call_as_agent(&task, "setStatus", report("s4", 1, "done"))
            .await,
    );
    let shown = read(&b, &id).await;
    ev.step(
        "caller-bound authority, stale refusal and no-op repeats",
        json!({"foreign":"notAuthorized","ownEvent":true,"repeatEvent":null,"repeatRevision":true,"stale":"staleRevision","forged":"invalidInput"}),
        json!({"foreign":foreign.code(),"ownEvent":own["eventSeq"].is_u64(),"repeatEvent":repeat["eventSeq"],"repeatRevision":repeat["revision"] == own["revision"],"stale":stale.code(),"forged":forged.code()}),
        Some(&own),
    );
    ev.step(
        "agent prose adds a blocker but the engine phase stays observed",
        json!({"phase":"working","agentBlocked":true,"reports":1}),
        json!({"phase":phase(&shown),"agentBlocked":attention_kinds(&shown).contains(&"agentBlocked".to_owned()),"reports":count(&event_kinds(&b,&id).await,"statusReported")}),
        Some(&shown),
    );
    f.peer.release(&key);
    ev.write();
    stop(b).await;
}

/// Case 14: editing the brief during a run keeps the sent snapshot and the
/// original message; Send update delivers the new revision once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn brief_edit_keeps_sent_snapshot_and_send_update_delivers_once() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("brief-edit-send-update");
    space(&b, &f, "project-orbit", "orbit").await;
    let (id, key) = ticket(&b, "ticket-a", "project-orbit", "Edit while running").await;
    f.peer.hold(&key);
    mv(&b, "r1", &id, "ready").await;
    let working = wait_phase(&b, &id, "working").await;
    let task = worker(&working).unwrap();
    let sent = launches(&working)[0].clone();
    let edited = ok(
        &b.c,
        "BoardUpdate",
        json!({"requestId":"edit-1","workItemId":id,"expectedRevision":working["view"]["item"]["revision"],"changes":{"description":BRIEF_V2}}),
    )
    .await;
    let after_edit = read(&b, &id).await;
    let original = launches(&after_edit)
        .into_iter()
        .find(|launch| launch["id"] == sent["id"])
        .unwrap();
    ev.step(
        "the sent snapshot is immutable while the brief moves on",
        json!({"briefRevision":2,"sentDescription":BRIEF,"sameHash":true,"prompts":1}),
        json!({"briefRevision":edited["item"]["briefRevision"],"sentDescription":original["sentDescription"],"sameHash":original["instructionHash"] == sent["instructionHash"],"prompts":f.peer.for_key(&key).len()}),
        Some(&original),
    );
    let request = json!({"requestId":"update-1","workItemId":id,"expectedRevision":edited["revision"],"mode":"queue"});
    let update = ok(&b.c, "BoardSendUpdate", request.clone()).await;
    let replay = ok(&b.c, "BoardSendUpdate", request).await;
    let queued = b
        .core
        .doc_host
        .open(&task)
        .unwrap()
        .doc()
        .read_queue()
        .unwrap()
        .iter()
        .filter(|row| row.id == update["launch"]["messageId"].as_str().unwrap())
        .count();
    ev.step(
        "Send update queues one BriefUpdate through the ordinary queue",
        json!({"kind":"briefUpdate","from":1,"to":2,"identicalReplay":true,"queuedRows":1}),
        json!({"kind":update["launch"]["kind"],"from":update["fromBriefRevision"],"to":update["toBriefRevision"],"identicalReplay":replay == update,"queuedRows":queued}),
        Some(&update),
    );
    f.peer.release(&key);
    wait_for(&b, &id, "the update to be delivered", |_| {
        f.peer.count(&key, "brief update") == 1
    })
    .await;
    let first_message = transcript(&b, &task)
        .into_iter()
        .find(|entry| entry.id == sent["messageId"].as_str().unwrap())
        .expect("the original message");
    let again = refused(
        &b.c,
        "BoardSendUpdate",
        json!({"requestId":"update-2","workItemId":id,"expectedRevision":edited["revision"],"mode":"queue"}),
    )
    .await;
    ev.step(
        "the original message is unchanged and an unchanged brief is not resent",
        json!({"originalUnchanged":true,"updates":1,"secondSend":"invalidInput"}),
        json!({"originalUnchanged":entry_text(&first_message) == sent["instructionText"].as_str().unwrap(),"updates":f.peer.count(&key,"brief update"),"secondSend":again.code()}),
        None,
    );
    ev.write();
    stop(b).await;
}

/// Case 15: "start it and report back here" delivers the outcome once to
/// the source chat, even after the source is archived and LUNA restarts,
/// and never wakes the source agent when only a report was requested.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn report_back_is_delivered_once_across_restart() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("report-back-once");
    let path = space(&b, &f, "project-orbit", "orbit").await;
    let source = "a9999999-9999-4999-8999-999999999999";
    main_chat(&b, "project-orbit", source, &path).await;
    let (id, _key) = ticket(&b, "ticket-a", "project-orbit", "Report back when done").await;
    let start = ok(
        &b.c,
        "BoardStart",
        json!({"requestId":"start-1","workItemId":id,"callerTaskId":source,"followUp":"reportBack"}),
    )
    .await;
    let idle = wait_phase(&b, &id, "idle").await;
    task_command(&b, source, json!({"kind":"setArchived","archived":true})).await;
    let done = b
        .rpc
        .board_qa_mark_done(
            &id,
            idle["intent"]["executionGeneration"].as_u64().unwrap(),
            idle["intent"]["intentRevision"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    let reports = |b: &Boot| {
        transcript(b, source)
            .into_iter()
            .filter(|entry| entry.id.starts_with("board-result-"))
            .map(|entry| entry_text(&entry))
            .collect::<Vec<_>>()
    };
    let delivered = reports(&b);
    ev.step(
        "the worker is a side chat of the source; Done delivers one report there",
        json!({"outcome":"started","parent":source,"done":true,"reports":1,"completed":true}),
        json!({"outcome":start["outcome"]["kind"],"parent":start["worker"]["mainParentId"],"done":done.is_some(),"reports":delivered.len(),"completed":delivered.first().is_some_and(|text| text.contains("completed"))}),
        Some(&start),
    );
    stop(b).await;
    let b = boot(&f);
    b.rpc.board_qa_reconcile().await.unwrap();
    for _ in 0..3 {
        b.rpc.board_lifecycle_tick().await.unwrap();
    }
    let detail = read(&b, &id).await;
    let subscription = detail["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["taskId"] == source)
        .map(|link| link["resultSubscriptions"][0].clone())
        .unwrap_or(Value::Null);
    ev.step(
        "after restart the report is still delivered exactly once and the source stayed idle",
        json!({"reports":1,"events":1,"delivered":true,"sourceTurns":0}),
        json!({"reports":reports(&b).len(),"events":count(&event_kinds(&b,&id).await,"resultDelivered"),"delivered":subscription["deliveredId"].is_string(),"sourceTurns":f.peer.for_key("chat").len()}),
        Some(&detail),
    );
    ev.write();
    stop(b).await;
}

/// Case 16: deferred settings hold Ready with a visible reason and start
/// only after they are chosen; nothing is guessed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_settings_hold_ready_until_chosen() {
    let f = fixture();
    let b = boot(&f);
    let mut ev = Evidence::new("deferred-settings-unresolved");
    space(&b, &f, "project-orbit", "orbit").await;
    let saved = ok(
        &b.c,
        "BoardCreate",
        json!({"requestId":"ticket-a","projectId":"project-orbit","title":"Choose settings later","description":BRIEF,"implementation":{"deferred":true}}),
    )
    .await;
    let id = saved["workItemId"].as_str().unwrap().to_owned();
    let key = saved["key"].as_str().unwrap().to_owned();
    f.peer.hold(&key);
    let moved = mv(&b, "ready-1", &id, "ready").await;
    let held = read(&b, &id).await;
    ev.step(
        "Ready holds with the unresolved fields; no chat, no prompt",
        json!({"effect":"queued","reason":"settingsUnresolved","fields":["harness","model","permissionMode"],"attention":true,"chat":null,"prompts":0}),
        json!({"effect":moved["effect"]["kind"],"reason":held["view"]["queueReason"]["kind"],"fields":held["view"]["queueReason"]["fields"],"attention":attention_kinds(&held).contains(&"settingsUnresolved".to_owned()),"chat":worker(&held),"prompts":f.peer.prompts().len()}),
        Some(&held),
    );
    ok(
        &b.c,
        "BoardUpdate",
        json!({"requestId":"choose","workItemId":id,"expectedRevision":held["view"]["item"]["revision"],"changes":{"implementation":{"harness":"mock","model":"fixture-pro","permissionMode":"ask"}}}),
    )
    .await;
    let started = wait_phase(&b, &id, "working").await;
    let launch = launches(&started)[0].clone();
    ev.step(
        "after the choice the ticket starts once with exactly the chosen settings",
        json!({"firstStarts":1,"model":"fixture-pro","permissionMode":"ask","reasoning":"high"}),
        json!({"firstStarts":f.peer.count(&key,"first start"),"model":launch["resolved"]["model"],"permissionMode":launch["resolved"]["permissionMode"],"reasoning":launch["resolved"]["reasoning"]}),
        Some(&launch),
    );
    f.peer.release(&key);
    ev.write();
    stop(b).await;
}
