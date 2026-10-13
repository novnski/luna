//! PB-4A feature groups E2E: the real engine over the in-memory RPC
//! transport → the `Board*` dispatcher → the feature-group service and the
//! Board store, with two disposable Git repositories registered as two
//! Spaces (a home repository and a related one), a third plain project, a
//! deterministic protocol peer, and an engine restart. Contract:
//! docs/pending-tasks/devin-project-board/pb-4a/README.md.
//!
//! Failure cases, written before the implementation (README F1–F12):
//!
//! 1. Creating a group or linking a ticket changes a member ticket (revision,
//!    column, project, key), adds a chat, launch, task link or Space, or
//!    renames a Space.
//! 2. Every member reaching Done accepts the group automatically, or
//!    `canAccept` turns true while an integration gate is still pending.
//! 3. `accept` succeeds while a member is not Done, is blocked, is deleted,
//!    or a gate is pending, failed or stale.
//! 4. A member's project, column, phase, source, working chat or checkout
//!    differs from that ticket's own `BoardRead`; two repositories on
//!    different branches show one branch; a related member is shown under
//!    the home Space.
//! 5. A gate passed or an acceptance recorded before a member reopened still
//!    counts.
//! 6. An open blocker outside the group is hidden, or stays listed once the
//!    blocker is Done.
//! 7. A retried mutation applies twice; the same key with another payload is
//!    accepted; a stale expected revision overwrites the group.
//! 8. Restart loses the group, its gates, acceptance or history.
//! 9. The related repository's board misses the group, or an unrelated
//!    project's board shows it.
//! 10. Unlinking a member or deleting the group edits the ticket; a deleted
//!     ticket silently disappears or counts as Done.
//! 11. Linking a missing ticket half-writes the group.
//! 12. A member's review run is re-derived instead of shown as its own
//!     review owner records it.
//!
//! Every scenario writes a JSON manifest (steps, expected/observed, SHA-256
//! of snapshots) under `target/qa/board/feature-groups/`.
#![cfg(unix)]

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, EngineRpc, HarnessRegistry};
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

const WAIT: Duration = Duration::from_secs(30);
const APP_BRANCH: &str = "feature/app-login";
const API_BRANCH: &str = "feature/api-tokens";
const BRIEF: &str = "Implement the slice.\n\n## Acceptance criteria\n- The change is covered by its repository's checks\n";

// ---------------------------------------------------------------------------
// Deterministic protocol peer: every turn completes at once.
// ---------------------------------------------------------------------------

struct Peer(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Feature group protocol peer"
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
        Ok(vec![Model {
            id: "fixture".into(),
            label: "fixture".into(),
            description: None,
            reasoning_levels: vec![ReasoningLevel::Low, ReasoningLevel::High],
            options: vec![],
            first_seen_at_ms: None,
        }])
    }
    async fn run(
        &self,
        request: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.0.lock().unwrap().push(request.prompt.clone());
        let session = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("native-{}", uuid::Uuid::new_v4()));
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: request.model.clone().unwrap_or_else(|| "fixture".into()),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: session.clone(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "Done with this slice.".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session),
            }),
        ])
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
    prompts: Arc<Mutex<Vec<String>>>,
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
        prompts: Arc::new(Mutex::new(Vec::new())),
    }
}

struct Boot {
    core: EngineCore,
    rpc: Arc<EngineRpc>,
    c: RpcClient,
}

fn boot(f: &Fixture) -> Boot {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Peer(f.prompts.clone())));
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

fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "LUNA QA")
        .env("GIT_AUTHOR_EMAIL", "qa@example.invalid")
        .env("GIT_COMMITTER_NAME", "LUNA QA")
        .env("GIT_COMMITTER_EMAIL", "qa@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A disposable repository on its own feature branch.
fn repository(f: &Fixture, folder: &str, branch: &str) -> PathBuf {
    let repo = f.projects.join(folder);
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), format!("# {folder}\n")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    git(&repo, &["checkout", "-q", "-b", branch]);
    repo
}

async fn space(b: &Boot, id: &str, path: &Path) {
    ok(
        &b.c,
        "Mutate",
        json!({"op":"createSpace","spaceId":id,"deviceId":b.core.device_id,"path":path.to_string_lossy()}),
    )
    .await;
}

fn spaces(b: &Boot) -> Value {
    let mut rows: Vec<Value> = b
        .core
        .workspace
        .read_spaces()
        .unwrap()
        .into_iter()
        .map(|space| json!({"id":space.id,"name":space.display_name(),"path":space.path,"deviceId":space.device_id}))
        .collect();
    rows.sort_by_key(|row| row["id"].as_str().unwrap_or_default().to_owned());
    Value::Array(rows)
}

/// A ticket the user accepts by hand (no automatic reviewer): Backlog, no
/// chat. Returns (id, key).
async fn ticket(b: &Boot, request_id: &str, project: &str, title: &str) -> (String, String) {
    let receipt = ok(
        &b.c,
        "BoardCreate",
        json!({"requestId":request_id,"projectId":project,"title":title,"description":BRIEF,
               "completion":{"mode":"manualAcceptance"},"review":{"disabled":true}}),
    )
    .await;
    (
        receipt["workItemId"].as_str().unwrap().to_owned(),
        receipt["key"].as_str().unwrap().to_owned(),
    )
}

async fn read(b: &Boot, id: &str) -> Value {
    ok(
        &b.c,
        "BoardRead",
        json!({"workItemId":id,"historyLimit":100}),
    )
    .await
}

/// What a group must never change on a member ticket.
fn identity(detail: &Value) -> Value {
    json!({
        "id": detail["view"]["item"]["id"],
        "key": detail["view"]["item"]["key"],
        "project": detail["view"]["item"]["projectId"],
        "revision": detail["view"]["item"]["revision"],
        "column": detail["view"]["item"]["column"],
        "links": detail["links"].as_array().map_or(0, Vec::len),
        "launches": detail["launches"].as_array().map_or(0, Vec::len),
        "reviews": detail["reviewRuns"].as_array().map_or(0, Vec::len),
        "dependencies": detail["dependencies"],
    })
}

fn worker(detail: &Value) -> Option<String> {
    detail["links"].as_array()?.iter().find_map(|link| {
        (link["role"] == "working" && link["active"] == true)
            .then(|| link["taskId"].as_str().unwrap().to_owned())
    })
}

async fn chat_count(b: &Boot) -> usize {
    let mut watch = b.c.subscribe("WatchChats", json!({})).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap();
    first.as_array().map_or(0, Vec::len)
}

/// Poll the ticket (driving the production lifecycle sweep) until `done`.
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
                detail["view"]["phase"], detail["intent"]
            );
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// The ticket's ordinary workflow: Backlog → Ready starts its own chat in
/// its own checkout; once the turn ends, the user accepts it into Done.
async fn complete(b: &Boot, id: &str, tag: &str) -> Value {
    ok(
        &b.c,
        "BoardMove",
        json!({"requestId":format!("{tag}-ready"),"workItemId":id,"to":"ready"}),
    )
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        b.rpc.board_lifecycle_tick().await.unwrap();
        match call(
            &b.c,
            "BoardControl",
            json!({"requestId":format!("{tag}-accept"),"workItemId":id,"action":"accept"}),
        )
        .await
        {
            Ok(_) => break,
            // Still working, or a concurrent lifecycle commit: try again.
            Err(error) => {
                let error = board_error(error);
                assert!(
                    matches!(error.code().as_str(), "invalidTransition" | "busy"),
                    "accepting {id} failed unexpectedly: {error:?}"
                );
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("{id} never became acceptable");
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    wait_for(b, id, "Done", |detail| {
        detail["view"]["item"]["column"] == "done"
    })
    .await
}

async fn groups(b: &Boot, scope: Value) -> Vec<Value> {
    ok(&b.c, "BoardFeatureGroups", json!({"scope":scope})).await["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn group(b: &Boot, id: &str) -> Value {
    let found = ok(
        &b.c,
        "BoardFeatureGroups",
        json!({"scope":{"kind":"allProjects"},"groupId":id,"includeArchived":true}),
    )
    .await["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(found.len(), 1, "group {id} must be listed once: {found:?}");
    found[0].clone()
}

async fn mutate(b: &Boot, request_id: &str, action: Value) -> Value {
    ok(
        &b.c,
        "BoardFeatureGroupMutate",
        json!({"requestId":request_id,"action":action}),
    )
    .await
}

async fn mutate_refused(b: &Boot, request_id: &str, action: Value) -> BoardError {
    refused(
        &b.c,
        "BoardFeatureGroupMutate",
        json!({"requestId":request_id,"action":action}),
    )
    .await
}

fn project(id: &str) -> Value {
    json!({"kind":"project","projectId":id})
}

fn unmet_kinds(view: &Value) -> Vec<String> {
    view["unmet"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|reason| {
            format!(
                "{}{}",
                reason["kind"].as_str().unwrap_or("?"),
                reason["key"]
                    .as_str()
                    .or(reason["gateId"].as_str())
                    .map(|what| format!(":{what}"))
                    .unwrap_or_default()
            )
        })
        .collect()
}

fn member<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["members"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|member| member["workItemId"] == id)
        .unwrap_or_else(|| panic!("{id} is not a member: {view}"))
}

fn gate_status(view: &Value) -> Vec<Value> {
    view["gates"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|gate| json!({"id":gate["gate"]["id"],"status":gate["status"]}))
        .collect()
}

fn history_kinds(view: &Value) -> Vec<String> {
    view["group"]["history"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["kind"]["kind"].as_str().map(str::to_owned))
        .collect()
}

/// A member exactly as its own owners report it: the ticket's `BoardRead`,
/// its working chat's checkout and its repository's checked-out branch.
fn owner_truth(b: &Boot, detail: &Value, branch: &str) -> Value {
    let worker = worker(detail);
    let cwd = worker
        .as_deref()
        .and_then(|task| b.core.workspace.chat(task).unwrap())
        .and_then(|chat| chat.cwd);
    json!({
        "project": detail["view"]["item"]["projectId"],
        "key": detail["view"]["item"]["key"],
        "column": detail["view"]["item"]["column"],
        "phase": detail["view"]["phase"],
        "source": "local",
        "worker": worker,
        "cwd": cwd,
        "liveBranch": branch,
        "generation": detail["intent"]["executionGeneration"],
    })
}

/// The checkout a member reports, resolved on disk.
fn checkout_dir(member: &Value) -> Option<PathBuf> {
    Path::new(member["checkout"]["cwd"].as_str()?)
        .canonicalize()
        .ok()
}

fn member_truth(member: &Value) -> Value {
    json!({
        "project": member["projectId"],
        "key": member["key"],
        "column": member["column"],
        "phase": member["phase"],
        "source": member["source"],
        "worker": member["workingChat"]["taskId"].as_str(),
        "cwd": member["checkout"]["cwd"],
        "liveBranch": member["checkout"]["liveBranch"],
        "generation": member["executionGeneration"],
    })
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
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/feature-groups");
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
            .unwrap_or_else(|| json!({"suite":"board_feature_groups_e2e","scenarios":{}}));
        manifest["suite"] = json!("board_feature_groups_e2e");
        manifest["contract"] = json!("docs/pending-tasks/devin-project-board/pb-4a/README.md");
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

/// Cases 1–11: one group homed in `app` with a member from the related `api`
/// repository; children finishing never accept it; acceptance is explicit,
/// bound to the members' state, survives restart, and unlinking or deleting
/// the group never touches a ticket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn feature_group_spans_two_repositories_and_is_accepted_only_explicitly() {
    let f = fixture();
    let app_repo = repository(&f, "app", APP_BRANCH);
    let api_repo = repository(&f, "api", API_BRANCH);
    let docs_dir = f.projects.join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    let b = boot(&f);
    let mut ev = Evidence::new("two-repositories-explicit-acceptance");
    space(&b, "project-app", &app_repo).await;
    space(&b, "project-api", &api_repo).await;
    space(&b, "project-docs", &docs_dir).await;
    let (app1, app1_key) = ticket(&b, "app-1", "project-app", "Login screen uses tokens").await;
    let (api1, api1_key) = ticket(&b, "api-1", "project-api", "Issue login tokens").await;
    let (api2, api2_key) = ticket(&b, "api-2", "project-api", "Rotate the signing key").await;
    ok(
        &b.c,
        "BoardSetDependency",
        json!({"requestId":"api-1-after-api-2","workItemId":api1,"blockedById":api2,"present":true}),
    )
    .await;

    // ---- create: zero effect on members, Spaces, chats ----
    let before = json!({
        "app1": identity(&read(&b, &app1).await),
        "api1": identity(&read(&b, &api1).await),
        "spaces": spaces(&b),
        "chats": chat_count(&b).await,
        "prompts": f.prompts.lock().unwrap().len(),
    });
    let created = mutate(
        &b,
        "create-login",
        json!({"kind":"create","projectId":"project-app","title":"Login across app and API",
               "acceptance":"- A user signs in with a token the API issued\n- Both repositories' checks pass",
               "gates":["Cross-repository login E2E"],"members":[app1_key, api1_key]}),
    )
    .await;
    let group_id = created["receipt"]["groupId"].as_str().unwrap().to_owned();
    let after = json!({
        "app1": identity(&read(&b, &app1).await),
        "api1": identity(&read(&b, &api1).await),
        "spaces": spaces(&b),
        "chats": chat_count(&b).await,
        "prompts": f.prompts.lock().unwrap().len(),
    });
    ev.step(
        "creating and linking leaves every ticket, Space, chat and provider untouched",
        before.clone(),
        after,
        Some(&created),
    );
    let view = group(&b, &group_id).await;
    let members: Vec<Value> = [app1.as_str(), api1.as_str()]
        .iter()
        .map(|id| {
            let m = member(&view, id);
            json!({"key": m["key"], "project": m["projectId"], "projectName": m["projectName"], "branch": m["checkout"]["liveBranch"], "checkout": m["checkout"]["source"], "column": m["column"], "phase": m["phase"]})
        })
        .collect();
    ev.step(
        "the group is homed in app; each member keeps its own repository and branch",
        json!({
            "home": "project-app", "homeName": "app", "revision": 1,
            "members": [
                {"key": app1_key, "project": "project-app", "projectName": "app", "branch": APP_BRANCH, "checkout": "project", "column": "backlog", "phase": "notStarted"},
                {"key": api1_key, "project": "project-api", "projectName": "api", "branch": API_BRANCH, "checkout": "project", "column": "backlog", "phase": "notStarted"},
            ],
            "repositories": 2,
        }),
        json!({
            "home": view["group"]["projectId"], "homeName": view["projectName"], "revision": view["group"]["revision"],
            "members": members,
            "repositories": view["summary"]["repositories"],
        }),
        Some(&view),
    );
    ev.step(
        "unmet lists both unfinished tickets, the outside blocker and the pending gate",
        json!({
            "unmet": [format!("memberNotDone:{app1_key}"), format!("memberNotDone:{api1_key}"), format!("memberBlocked:{api1_key}"), "gatePending:g1"],
            "blockers": [{"key": api2_key, "inGroup": false}],
            "acceptance": "notAccepted", "canAccept": false,
        }),
        json!({
            "unmet": unmet_kinds(&view),
            "blockers": member(&view, &api1)["blockedBy"].as_array().unwrap().iter().map(|blocker| json!({"key": blocker["key"], "inGroup": blocker["inGroup"]})).collect::<Vec<_>>(),
            "acceptance": view["acceptance"], "canAccept": view["canAccept"],
        }),
        Some(&view),
    );
    let early = mutate_refused(
        &b,
        "accept-early",
        json!({"kind":"accept","groupId":group_id,"expectedRevision":1}),
    )
    .await;
    ev.step(
        "accept is refused with every reason and writes nothing",
        json!({"code": "invalidInput", "field": "acceptance", "names": true, "revision": 1}),
        json!({
            "code": early.code(),
            "field": match &early { BoardError::InvalidInput { field, .. } => field.clone(), _ => String::new() },
            "names": early.message().contains(&app1_key) && early.message().contains(&api2_key),
            "revision": group(&b, &group_id).await["group"]["revision"],
        }),
        None,
    );

    // ---- scope: the related repository sees it, an unrelated one does not ----
    let api_scope = groups(&b, project("project-api")).await;
    let docs_scope = groups(&b, project("project-docs")).await;
    let app_scope = groups(&b, project("project-app")).await;
    ev.step(
        "the related repository lists the group as not home; an unrelated project lists nothing",
        json!({"api": [[group_id.clone(), false]], "docs": 0, "app": [[group_id.clone(), true]]}),
        json!({
            "api": api_scope.iter().map(|v| json!([v["group"]["id"], v["home"]])).collect::<Vec<_>>(),
            "docs": docs_scope.len(),
            "app": app_scope.iter().map(|v| json!([v["group"]["id"], v["home"]])).collect::<Vec<_>>(),
        }),
        None,
    );

    // ---- dependency aggregation follows the blocker's own state ----
    complete(&b, &api2, "api-2").await;
    let view = group(&b, &group_id).await;
    ev.step(
        "the blocker reaching Done clears memberBlocked; the group still waits",
        json!([
            format!("memberNotDone:{app1_key}"),
            format!("memberNotDone:{api1_key}"),
            "gatePending:g1"
        ]),
        json!(unmet_kinds(&view)),
        Some(&view),
    );

    // ---- children Done: the group is still not accepted ----
    complete(&b, &app1, "app-1").await;
    complete(&b, &api1, "api-1").await;
    let view = group(&b, &group_id).await;
    let (app1_done, api1_done) = (read(&b, &app1).await, read(&b, &api1).await);
    ev.step(
        "every child Done does not accept the group; only the gate remains",
        json!({"unmet": ["gatePending:g1"], "acceptance": "notAccepted", "canAccept": false, "done": 2, "record": null}),
        json!({"unmet": unmet_kinds(&view), "acceptance": view["acceptance"], "canAccept": view["canAccept"], "done": view["summary"]["done"], "record": view["group"]["acceptanceRecord"]}),
        Some(&view),
    );
    ev.step(
        "each member equals its own BoardRead, worker and repository branch",
        json!([
            owner_truth(&b, &app1_done, APP_BRANCH),
            owner_truth(&b, &api1_done, API_BRANCH)
        ]),
        json!([
            member_truth(member(&view, &app1)),
            member_truth(member(&view, &api1))
        ]),
        Some(&view),
    );
    ev.step(
        "the two workers are distinct chats, each in its own repository",
        json!({"distinct": true, "appRepo": true, "apiRepo": true, "appSource": "workingChat", "apiSource": "workingChat", "review": [null, null]}),
        json!({
            "distinct": worker(&app1_done) != worker(&api1_done),
            "appRepo": checkout_dir(member(&view, &app1)) == app_repo.canonicalize().ok(),
            "apiRepo": checkout_dir(member(&view, &api1)) == api_repo.canonicalize().ok(),
            "appSource": member(&view, &app1)["checkout"]["source"],
            "apiSource": member(&view, &api1)["checkout"]["source"],
            "review": [member(&view, &app1)["review"].clone(), member(&view, &api1)["review"].clone()],
        }),
        Some(&view),
    );
    let gated = mutate_refused(
        &b,
        "accept-gated",
        json!({"kind":"accept","groupId":group_id,"expectedRevision":1}),
    )
    .await;
    ev.step(
        "accept is still refused while the integration gate is pending",
        json!({"code": "invalidInput", "mentionsGate": true}),
        json!({"code": gated.code(), "mentionsGate": gated.message().contains("Cross-repository login E2E")}),
        None,
    );

    // ---- gates are explicit and bound to the members' state ----
    let failed = mutate(
        &b,
        "gate-failed",
        json!({"kind":"recordGate","groupId":group_id,"expectedRevision":1,"gateId":"g1","state":"failed","note":"Token refresh 401"}),
    )
    .await;
    ev.step(
        "a failed gate is unmet",
        json!({"revision": 2, "unmet": ["gateFailed:g1"], "canAccept": false}),
        json!({"revision": failed["receipt"]["revision"], "unmet": unmet_kinds(&failed["group"]), "canAccept": failed["group"]["canAccept"]}),
        Some(&failed),
    );
    let passed = mutate(
        &b,
        "gate-passed",
        json!({"kind":"recordGate","groupId":group_id,"expectedRevision":2,"gateId":"g1","state":"passed",
               "evidence":[{"label":"login-e2e.log","path":"target/qa/login-e2e.log"}]}),
    )
    .await;
    ev.step(
        "a passed gate leaves nothing unmet; acceptance is still the user's call",
        json!({"revision": 3, "unmet": [], "canAccept": true, "acceptance": "notAccepted", "gates": [{"id":"g1","status":"passed"}]}),
        json!({"revision": passed["receipt"]["revision"], "unmet": unmet_kinds(&passed["group"]), "canAccept": passed["group"]["canAccept"], "acceptance": passed["group"]["acceptance"], "gates": gate_status(&passed["group"])}),
        Some(&passed),
    );
    let accepted = mutate(
        &b,
        "accept-1",
        json!({"kind":"accept","groupId":group_id,"expectedRevision":3}),
    )
    .await;
    ev.step(
        "explicit accept records an acceptance bound to both members",
        json!({"revision": 4, "changed": true, "acceptance": "accepted", "canAccept": false, "basis": 2}),
        json!({
            "revision": accepted["receipt"]["revision"], "changed": accepted["receipt"]["changed"],
            "acceptance": accepted["group"]["acceptance"], "canAccept": accepted["group"]["canAccept"],
            "basis": accepted["group"]["group"]["acceptanceRecord"]["basis"].as_array().map_or(0, Vec::len),
        }),
        Some(&accepted),
    );

    // ---- idempotency and concurrency ----
    let replay = mutate(
        &b,
        "accept-1",
        json!({"kind":"accept","groupId":group_id,"expectedRevision":3}),
    )
    .await;
    let conflict = mutate_refused(
        &b,
        "accept-1",
        json!({"kind":"reopen","groupId":group_id,"expectedRevision":4}),
    )
    .await;
    let stale = mutate_refused(
        &b,
        "rename-stale",
        json!({"kind":"update","groupId":group_id,"expectedRevision":3,"title":"Stale writer"}),
    )
    .await;
    let again = mutate(
        &b,
        "accept-2",
        json!({"kind":"accept","groupId":group_id,"expectedRevision":4}),
    )
    .await;
    ev.step(
        "a retry returns the stored receipt; another payload conflicts; a stale writer is refused; a repeat accept is a no-op",
        json!({"replay": accepted["receipt"].clone(), "conflict": "idempotencyConflict", "stale": "invalidInput", "again": {"changed": false, "revision": 4}, "title": "Login across app and API"}),
        json!({
            "replay": replay["receipt"], "conflict": conflict.code(), "stale": stale.code(),
            "again": {"changed": again["receipt"]["changed"], "revision": again["receipt"]["revision"]},
            "title": group(&b, &group_id).await["group"]["title"],
        }),
        None,
    );

    // ---- a member reopening makes the acceptance and the gate stale ----
    ok(
        &b.c,
        "BoardMove",
        json!({"requestId":"app-1-reopen","workItemId":app1,"to":"backlog"}),
    )
    .await;
    wait_for(&b, &app1, "reopened", |detail| {
        detail["view"]["item"]["column"] == "backlog"
    })
    .await;
    let view = group(&b, &group_id).await;
    ev.step(
        "a reopened member: acceptance and gate stale, record unchanged",
        json!({"acceptance": "stale", "gates": [{"id":"g1","status":"stale"}], "unmet": [format!("memberNotDone:{app1_key}"), "gateStale:g1".to_owned()], "revision": 4, "canAccept": false, "recordKept": true}),
        json!({"acceptance": view["acceptance"], "gates": gate_status(&view), "unmet": unmet_kinds(&view), "revision": view["group"]["revision"], "canAccept": view["canAccept"], "recordKept": view["group"]["acceptanceRecord"].is_object()}),
        Some(&view),
    );

    // ---- restart keeps the record ----
    let record_before = view["group"].clone();
    stop(b).await;
    let b = boot(&f);
    b.rpc.board_qa_reconcile().await.unwrap();
    let view = group(&b, &group_id).await;
    ev.step(
        "restart keeps the group, gates, acceptance record and history",
        json!({"record": record_before.clone(), "acceptance": "stale"}),
        json!({"record": view["group"], "acceptance": view["acceptance"]}),
        Some(&view),
    );

    // ---- unlink: the ticket is untouched, the acceptance is withdrawn ----
    let api1_before = identity(&read(&b, &api1).await);
    let removed = mutate(
        &b,
        "unlink-api-1",
        json!({"kind":"removeMember","groupId":group_id,"expectedRevision":4,"workItemId":api1}),
    )
    .await;
    ev.step(
        "unlinking leaves the ticket as it was and clears the group's acceptance",
        json!({"ticket": api1_before.clone(), "members": [app1.clone()], "record": null, "acceptance": "notAccepted", "history": ["memberRemoved", "acceptanceCleared"]}),
        json!({
            "ticket": identity(&read(&b, &api1).await),
            "members": removed["group"]["group"]["members"].as_array().unwrap().iter().map(|m| m["workItemId"].clone()).collect::<Vec<_>>(),
            "record": removed["group"]["group"]["acceptanceRecord"],
            "acceptance": removed["group"]["acceptance"],
            "history": history_kinds(&removed["group"]).into_iter().rev().take(2).rev().collect::<Vec<_>>(),
        }),
        Some(&removed),
    );
    ev.step(
        "the related repository no longer lists the group",
        json!(0),
        json!(groups(&b, project("project-api")).await.len()),
        None,
    );

    // ---- a deleted member stays visible and blocks acceptance ----
    let (app2, app2_key) = ticket(&b, "app-2", "project-app", "Remember the last account").await;
    let linked = mutate(
        &b,
        "link-app-2",
        json!({"kind":"addMember","groupId":group_id,"expectedRevision":5,"workItemId":app2_key}),
    )
    .await;
    ok(
        &b.c,
        "BoardDelete",
        json!({"requestId":"delete-app-2","workItemId":app2,"expectedRevision":1}),
    )
    .await;
    let view = group(&b, &group_id).await;
    ev.step(
        "a deleted ticket is shown as unavailable and keeps the group unaccepted",
        json!({"revision": 6, "availability": "deleted", "unavailable": true, "canAccept": false}),
        json!({
            "revision": linked["receipt"]["revision"],
            "availability": member(&view, &app2)["availability"],
            "unavailable": unmet_kinds(&view).contains(&format!("memberUnavailable:{app2_key}")),
            "canAccept": view["canAccept"],
        }),
        Some(&view),
    );
    let missing = mutate_refused(
        &b,
        "link-missing",
        json!({"kind":"addMember","groupId":group_id,"expectedRevision":6,"workItemId":"APP-999"}),
    )
    .await;
    let deleted_again = mutate_refused(
        &b,
        "link-deleted",
        json!({"kind":"addMember","groupId":group_id,"expectedRevision":6,"workItemId":app2}),
    )
    .await;
    ev.step(
        "linking a missing or deleted ticket is refused without a write",
        json!({"missing": "notFound", "deleted": "notFound", "revision": 6}),
        json!({"missing": missing.code(), "deleted": deleted_again.code(), "revision": group(&b, &group_id).await["group"]["revision"]}),
        None,
    );
    let unlinked = mutate(
        &b,
        "unlink-app-2",
        json!({"kind":"removeMember","groupId":group_id,"expectedRevision":6,"workItemId":app2_key}),
    )
    .await;
    ev.step(
        "a deleted ticket can still be unlinked by its key",
        json!({"revision": 7, "members": 1}),
        json!({"revision": unlinked["receipt"]["revision"], "members": unlinked["group"]["members"].as_array().map_or(0, Vec::len)}),
        None,
    );

    // ---- deleting the group leaves its tickets alone ----
    let app1_before = identity(&read(&b, &app1).await);
    let deleted = mutate(
        &b,
        "delete-group",
        json!({"kind":"delete","groupId":group_id,"expectedRevision":7}),
    )
    .await;
    ev.step(
        "deleting the group removes only the group",
        json!({"deleted": true, "group": null, "listed": 0, "ticket": app1_before.clone()}),
        json!({
            "deleted": deleted["receipt"]["deleted"],
            "group": deleted.get("group").cloned().unwrap_or(Value::Null),
            "listed": groups(&b, json!({"kind":"allProjects"})).await.len(),
            "ticket": identity(&read(&b, &app1).await),
        }),
        Some(&deleted),
    );
    ev.write();
    stop(b).await;
}

/// Case 12: a member's latest review run is shown exactly as its own review
/// owner records it (id, state, generation), and a ticket under review is
/// not Done for the group.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_review_runs_are_shown_as_their_owner_reports_them() {
    let f = fixture();
    let repo = repository(&f, "service", "feature/service-review");
    let b = boot(&f);
    let mut ev = Evidence::new("member-review-run");
    space(&b, "project-service", &repo).await;
    // Default completion: automatic checks and agent review.
    let receipt = ok(
        &b.c,
        "BoardCreate",
        json!({"requestId":"svc-1","projectId":"project-service","title":"Harden the token endpoint","description":BRIEF}),
    )
    .await;
    let id = receipt["workItemId"].as_str().unwrap().to_owned();
    let key = receipt["key"].as_str().unwrap().to_owned();
    ok(
        &b.c,
        "BoardMove",
        json!({"requestId":"svc-1-ready","workItemId":id,"to":"ready"}),
    )
    .await;
    wait_for(&b, &id, "the first turn to finish", |detail| {
        worker(detail).is_some()
            && detail["launches"]
                .as_array()
                .is_some_and(|launches| !launches.is_empty())
            && !matches!(
                detail["view"]["phase"].as_str(),
                Some("notStarted" | "queued" | "starting" | "working")
            )
    })
    .await;
    match call(
        &b.c,
        "BoardReviewStart",
        json!({"requestId":"svc-1-review","workItemId":id}),
    )
    .await
    {
        Ok(_) => {}
        // The engine already started this result's review.
        Err(error) => assert_eq!(board_error(error).code(), "busy"),
    }
    wait_for(&b, &id, "a review run", |detail| {
        detail["reviewRuns"]
            .as_array()
            .is_some_and(|runs| !runs.is_empty())
    })
    .await;
    let created = mutate(
        &b,
        "create-review-group",
        json!({"kind":"create","projectId":"project-service","title":"Token hardening","members":[key]}),
    )
    .await;
    let group_id = created["receipt"]["groupId"].as_str().unwrap().to_owned();
    // The run keeps moving; compare one consistent pair of reads.
    let deadline = tokio::time::Instant::now() + WAIT;
    let (view, detail) = loop {
        b.rpc.board_lifecycle_tick().await.unwrap();
        let view = group(&b, &group_id).await;
        let detail = read(&b, &id).await;
        let again = group(&b, &group_id).await;
        if view["members"] == again["members"] {
            let run = &detail["reviewRuns"][0];
            let review = &member(&view, &id)["review"];
            if review["reviewRunId"] == run["id"] && review["state"] == run["state"] {
                break (view, detail);
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the group's review summary never matched the review owner"
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
    };
    let run = &detail["reviewRuns"][0];
    let review = &member(&view, &id)["review"];
    ev.step(
        "the member's review is its latest run as the review owner records it",
        json!({
            "id": run["id"], "state": run["state"], "generation": run["executionGeneration"],
            "verdict": run["result"]["verdict"],
            "current": run["executionGeneration"] == detail["intent"]["executionGeneration"],
        }),
        json!({
            "id": review["reviewRunId"], "state": review["state"], "generation": review["executionGeneration"],
            "verdict": review.get("verdict").cloned().unwrap_or(Value::Null),
            "current": review["current"],
        }),
        Some(&view),
    );
    ev.step(
        "a ticket that has not passed review is not Done for the group",
        json!({"notDone": detail["view"]["item"]["column"] != "done", "canAccept": false}),
        json!({"notDone": unmet_kinds(&view).contains(&format!("memberNotDone:{key}")), "canAccept": view["canAccept"]}),
        Some(&view),
    );
    ev.write();
    stop(b).await;
}
