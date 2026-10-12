//! PB-1R board review E2E (automatic-review §9 scenarios 1–10): the real
//! engine over the in-memory RPC transport → the `Board*` dispatcher → the
//! lifecycle (A2), chat-model (A3) and review (A5) slices → deterministic
//! protocol peers with distinct harness identities, so a "Codex"
//! implementer is reviewed by a "Claude Code" reviewer in a real Git
//! checkout. Agent tool calls (`review_start` from the implementer,
//! `review_submit` from the reviewer) go through the engine's LUNA tool
//! router (`CrossTaskTools::invoke`) with the authority bound for that
//! chat's run — the path a provider MCP call takes after its adapter. No
//! live provider, network or GitHub write.
//!
//! Failure cases, written before the implementation (contract §11.3):
//!
//! 1. Main-ticket placement: the reviewer is not one side chat directly
//!    under the ticket chat; it gets a main-sidebar entry, inherits the
//!    ticket transcript (resumes a session) or runs with write access; a
//!    later round creates a second reviewer chat; the reviewer runs on the
//!    implementer's harness instead of the selected Claude Code model or the
//!    observed model is not read back; the primary chat's id, provider or
//!    history changes.
//! 2. Side-ticket placement: the reviewer nests under the side ticket or
//!    lands under any chat but the same main parent; the main parent (not
//!    the ticket) becomes the workflow owner.
//! 3. Repair loop: a seeded defect passes; findings arrive as a forged user
//!    message, twice, or not at all; the re-review covers a stale snapshot;
//!    Done happens without a passing review of the final diff or needs a
//!    human click; checks from an earlier snapshot count.
//! 4. Stale diff: files mutated during review still let the old verdict
//!    complete the ticket, or the new basis is never reviewed.
//! 5. Restart / lost responses: a retried or concurrent `review_start`
//!    creates a second run; a restart at the launch boundary creates a
//!    second reviewer chat or loses the owner; a restart at the delivery
//!    boundary delivers the repair request twice or never.
//! 6. Unresolved states: an unavailable reviewer model silently falls back
//!    to another model; omitted required checks, an unobserved model
//!    substitution, a reviewer turn that ends in prose without
//!    `review_submit`, or an exhausted repair cap still yield Done; another
//!    chat can submit a result for the run.
//! 7. Converted main chat: conversion or review replaces the conversation's
//!    identity/history, or the default reviewer (no profile configured) is
//!    not the implementation model in a separate reviewer side chat.
//! 8. Cleanup: the primary, its main parent or an unrelated sibling is
//!    archived; the reviewer is archived before Done/evidence are durable or
//!    while its turn runs; its views stay open; archived review details are
//!    unreadable or silently unarchived.
//! 9. Lost/partial cleanup: a reviewer still finishing its turn or a layout
//!    revision conflict is reported as cleaned up, re-runs the paid review,
//!    or journals success more than once.
//! 10. Stop during sibling review: moving the side ticket to Backlog misses
//!     the sibling reviewer, stops an unrelated sibling or the main chat, or
//!     a late passing result marks the ticket Done.
//! 11. Committed results: committing on main before the first review hides
//!     the implementation behind a new HEAD base; committing a repair makes
//!     its freshly reported checks stale against the retained review base.
//! 12. A newly failed check is hidden by an older passing check on the same
//!     diff, or an inherited reviewer model is silently substituted while
//!     an explicitly selected model would be rejected.
//! 13. Changing bytes beyond the prefix of a large binary artifact is
//!     missed by the stale-review guard even though the file size is stable.
//! 14. Checks for the old binary contents survive a fresh review, or Git's
//!     collapsed untracked-directory listing hides new nested source files.
//! 15. LUN-12: a Done ticket is deletable while its preserved working chat
//!     has a later live user turn, because the Done phase masks runtime work.
//!
//! Every scenario writes a JSON manifest (steps, expected/observed, SHA-256
//! of snapshots) under `target/qa/board/review/`.
#![cfg(unix)]

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::cross_task_tools::{CrossTaskAuthority, CrossTaskTools};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, PermissionMode, ReasoningLevel, RunRequest,
    RuntimeDelivery, SandboxLevel,
};
use luna_rpc::{RpcClient, RpcError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const IMPLEMENTER_MODEL: &str = "gpt-5.5-codex";
const REVIEWER_MODEL: &str = "opus-5.5";
const WAIT: Duration = Duration::from_secs(60);
const DEFECT: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n";
const FIXED: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";

// ---------------------------------------------------------------------------
// Deterministic protocol peers
// ---------------------------------------------------------------------------

/// Knobs a scenario flips on the reviewer peer.
#[derive(Default)]
struct PeerControl {
    /// The model the Claude peer reports in `SessionStarted` regardless of
    /// the request (an unobserved provider-side substitution).
    reviewer_reports: Mutex<Option<String>>,
    inherited_reviewer_reports: Mutex<Option<String>>,
}

/// One provider turn, held open until the scenario finishes it (or the
/// engine interrupts it through Stop).
struct Turn {
    harness: HarnessId,
    prompt: String,
    sandbox: SandboxLevel,
    permission_mode: PermissionMode,
    model: Option<String>,
    resume: Option<String>,
    interrupted: Arc<AtomicBool>,
    release: Option<oneshot::Sender<()>>,
}

impl Turn {
    fn is_packet(&self) -> bool {
        self.prompt.contains("# LUNA review packet")
    }

    fn is_repair(&self) -> bool {
        self.prompt.contains("[LUNA review result")
    }

    fn finish(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

struct Peer {
    id: HarnessId,
    models: Vec<Model>,
    turns: mpsc::UnboundedSender<Turn>,
    control: Arc<PeerControl>,
}

fn model(id: &str) -> Model {
    Model {
        id: id.into(),
        label: id.into(),
        description: None,
        reasoning_levels: vec![ReasoningLevel::Medium, ReasoningLevel::High],
        options: Vec::new(),
        first_seen_at_ms: None,
    }
}

#[async_trait]
impl Harness for Peer {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn display_name(&self) -> &str {
        match (self.id).kind() {
            luna_proto::BuiltinHarnessId::ClaudeCode => "Claude Code review peer",
            _ => "Codex implementation peer",
        }
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
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(self.models.clone())
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (release, released) = oneshot::channel();
        let interrupted = Arc::new(AtomicBool::new(false));
        let reported = match (self.id).kind() {
            luna_proto::BuiltinHarnessId::ClaudeCode => {
                self.control.reviewer_reports.lock().unwrap().clone()
            }
            luna_proto::BuiltinHarnessId::Codex
                if request.prompt.contains("# LUNA review packet") =>
            {
                self.control
                    .inherited_reviewer_reports
                    .lock()
                    .unwrap()
                    .clone()
            }
            _ => None,
        };
        let model = reported
            .or_else(|| request.model.clone())
            .unwrap_or_else(|| self.models[0].id.clone());
        let session_id = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("{:?}-{}", self.id, uuid::Uuid::new_v4()));
        let _ = self.turns.send(Turn {
            harness: self.id,
            prompt: request.prompt.clone(),
            sandbox: request.sandbox,
            permission_mode: request.permission_mode,
            model: request.model.clone(),
            resume: request.resume.clone(),
            interrupted: interrupted.clone(),
            release: Some(release),
        });
        let harness = self.id;
        let cwd = request.cwd.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let controls = controls;
            let _ = tx.send(Ok(AgentEvent::SessionStarted {
                harness,
                model,
                tools: Vec::new(),
                cwd,
                session_id: session_id.clone(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }));
            let _ = tx.send(Ok(AgentEvent::TextDelta {
                text: "Working.\n".into(),
            }));
            let status = tokio::select! {
                _ = released => {
                    let _ = tx.send(Ok(AgentEvent::TextDelta { text: "Finished.".into() }));
                    DoneStatus::Completed
                }
                _ = controls.interrupt.cancelled() => {
                    interrupted.store(true, Ordering::SeqCst);
                    DoneStatus::Interrupted
                }
            };
            let _ = tx.send(Ok(AgentEvent::Done {
                status,
                result: None,
                error: None,
                session_id: Some(session_id),
            }));
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

// ---------------------------------------------------------------------------
// Evidence manifest
// ---------------------------------------------------------------------------

fn sha256(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

struct Evidence {
    scenario: &'static str,
    steps: Vec<Value>,
}

impl Evidence {
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

    fn check(&mut self, name: &str, condition: bool, observed: Value) {
        assert!(condition, "{}: {name}: {observed}", self.scenario);
        self.steps.push(json!({
            "step": name,
            "expected": true,
            "observed": observed,
            "snapshotSha256": sha256(&observed),
            "passed": true,
        }));
    }

    fn write(self) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/review");
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
            "implementer": {"harness": "codex", "model": IMPLEMENTER_MODEL},
            "reviewer": {"harness": "claude-code", "model": REVIEWER_MODEL},
            "steps": self.steps,
        });
        let body = serde_json::to_vec_pretty(&scenario).unwrap();
        std::fs::write(dir.join(format!("{}.json", self.scenario)), &body).unwrap();
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Value = std::fs::read(&manifest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({"suite":"board_review_e2e","scenarios":{}}));
        manifest["suite"] = json!("board_review_e2e");
        manifest["contract"] = json!("docs/design/project-board/pb0-contract.md §5.5, §11.3");
        manifest["spec"] = json!("docs/pending-tasks/devin-project-board/automatic-review.md §9");
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
// Rig: engine, Git checkout, peers, restart
// ---------------------------------------------------------------------------

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

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    project: String,
    turns_tx: mpsc::UnboundedSender<Turn>,
    turns: mpsc::UnboundedReceiver<Turn>,
    pending: VecDeque<Turn>,
    control: Arc<PeerControl>,
    core: Option<EngineCore>,
    client: Option<RpcClient>,
    evidence: Evidence,
    /// Turns that are finished as soon as they arrive while waiting for
    /// another (e.g. a provider-side recovery of an interrupted turn).
    autofinish: Option<fn(&Turn) -> bool>,
}

impl Rig {
    async fn new(scenario: &'static str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("luna");
        let repo = dir.path().join("projects").join("lunar-app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 {\n    todo!()\n}\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("AGENTS.md"),
            "# Lunar app\n\nKeep functions pure. Run `cargo test` before review.\n",
        )
        .unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        let (turns_tx, turns) = mpsc::unbounded_channel();
        let mut rig = Self {
            _dir: dir,
            root,
            repo,
            project: "lunar-app".into(),
            turns_tx,
            turns,
            pending: VecDeque::new(),
            control: Arc::new(PeerControl::default()),
            core: None,
            client: None,
            evidence: Evidence {
                scenario,
                steps: Vec::new(),
            },
            autofinish: None,
        };
        rig.boot();
        let path = rig.repo.to_string_lossy().to_string();
        let device = rig.core().device_id.clone();
        rig.ok(
            "Mutate",
            json!({"op":"createSpace","spaceId":rig.project,"deviceId":device,"path":path}),
        )
        .await;
        rig
    }

    fn boot(&mut self) {
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(Peer {
            id: HarnessId::Codex,
            models: vec![model(IMPLEMENTER_MODEL)],
            turns: self.turns_tx.clone(),
            control: self.control.clone(),
        }));
        registry.register(Arc::new(Peer {
            id: HarnessId::ClaudeCode,
            models: vec![model(REVIEWER_MODEL), model("sonnet-5")],
            turns: self.turns_tx.clone(),
            control: self.control.clone(),
        }));
        let fixtures = self.root.parent().unwrap().join("provider-fixtures");
        let accounts = AgentAccountsConfig {
            data_dir: fixtures.join("accounts"),
            claude_config_dir: fixtures.join("claude"),
            claude_config_file: fixtures.join("claude.json"),
            codex_home: fixtures.join("codex"),
        };
        accounts.disable_claude_keychain_fallback().unwrap();
        std::fs::create_dir_all(&accounts.claude_config_dir).unwrap();
        std::fs::write(
            &accounts.claude_config_file,
            r#"{"oauthAccount":{"accountUuid":"review-fixture","emailAddress":"review@example.invalid"}}"#,
        )
        .unwrap();
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::local(&self.root).unwrap(),
                Arc::new(registry),
                HarnessId::Codex,
                None,
                accounts,
            )
            .unwrap();
        self.client = Some(luna_rpc::memory_client(core.rpc_service()));
        self.core = Some(core);
    }

    /// Graceful engine restart on the same data root (held turns end).
    async fn restart(&mut self) {
        let core = self.core.take().unwrap();
        core.shutdown().await;
        drop(self.client.take());
        drop(core);
        for turn in self.pending.drain(..) {
            turn.finish();
        }
        while let Ok(turn) = self.turns.try_recv() {
            turn.finish();
        }
        self.boot();
    }

    fn core(&self) -> &EngineCore {
        self.core.as_ref().unwrap()
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.client
            .as_ref()
            .unwrap()
            .call_as::<Value>(method, params)
            .await
    }

    async fn ok(&self, method: &str, params: Value) -> Value {
        match self.call(method, params.clone()).await {
            Ok(value) => value,
            Err(error) => panic!("{method} {params} failed: {error}"),
        }
    }

    /// The next turn matching `wanted` (others are kept for later).
    async fn turn(&mut self, what: &str, wanted: impl Fn(&Turn) -> bool) -> Turn {
        if let Some(index) = self.pending.iter().position(&wanted) {
            return self.pending.remove(index).unwrap();
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let turn = tokio::time::timeout_at(deadline, self.turns.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!("{}: no {what} turn within {WAIT:?}", self.evidence.scenario)
                })
                .expect("peer channel open");
            if wanted(&turn) {
                return turn;
            }
            if self.autofinish.is_some_and(|autofinish| autofinish(&turn)) {
                turn.finish();
                continue;
            }
            self.pending.push_back(turn);
        }
    }

    /// [`Self::turn`] for a ticket's turn; a timeout reports the ticket's
    /// engine view and intent (phase, queue reason, attention) for triage.
    async fn ticket_turn(
        &mut self,
        item: &str,
        what: &str,
        wanted: impl Fn(&Turn) -> bool,
    ) -> Turn {
        if let Some(index) = self.pending.iter().position(&wanted) {
            return self.pending.remove(index).unwrap();
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.turns.recv()).await {
                Ok(Some(turn)) if wanted(&turn) => return turn,
                Ok(Some(turn)) => {
                    if self.autofinish.is_some_and(|autofinish| autofinish(&turn)) {
                        turn.finish();
                    } else {
                        self.pending.push_back(turn);
                    }
                }
                Ok(None) => panic!("peer channel closed"),
                Err(_) => {
                    let detail = self.detail(item).await;
                    panic!(
                        "{}: no {what} turn within {WAIT:?}; view {:#} intent {:#}",
                        self.evidence.scenario,
                        json!({"phase": detail["view"]["phase"], "attention": detail["view"]["attention"],
                            "queueReason": detail["view"]["queueReason"], "column": detail["view"]["item"]["column"]}),
                        detail["intent"]
                    );
                }
            }
        }
    }

    /// Wait until a chat's turn has settled.
    async fn wait_idle(&self, chat: &str) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.core().sessions.active_turn_id(chat).is_some()
            || self.core().sessions.turn_in_flight(chat)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{chat} never settled"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// No matching turn arrives within `window`.
    async fn no_turn(&mut self, window: Duration, wanted: impl Fn(&Turn) -> bool) -> bool {
        if self.pending.iter().any(&wanted) {
            return false;
        }
        let deadline = tokio::time::Instant::now() + window;
        while let Ok(Some(turn)) = tokio::time::timeout_at(deadline, self.turns.recv()).await {
            let hit = wanted(&turn);
            self.pending.push_back(turn);
            if hit {
                return false;
            }
        }
        true
    }

    /// A LUNA tool call as `task` (the authority the engine binds for that
    /// chat's run; never a model-supplied identity).
    async fn tool(&self, task: &str, name: &str, args: Value) -> Result<Value, String> {
        let harness = self
            .core()
            .workspace
            .chat(task)
            .unwrap()
            .and_then(|chat| chat.config)
            .map(|config| config.harness);
        let authority = CrossTaskAuthority {
            caller_task_id: task.into(),
            local_environment_id: self.core().device_id.clone(),
            visible_project_ids: HashSet::from([self.project.clone()]),
            allow_projectless: false,
            granted_scopes: ["threads.read", "board.read", "board.write", "board.review"]
                .into_iter()
                .map(String::from)
                .collect(),
            explicit_user_request: false,
            delivery: if harness == Some(HarnessId::ClaudeCode) {
                RuntimeDelivery::ClaudeMcpTools
            } else {
                RuntimeDelivery::CodexMcpTools
            },
        };
        let tools = CrossTaskTools::new(self.core().rpc_service());
        match tools
            .invoke(&authority, name, args, &CancellationToken::new())
            .await
        {
            Ok(value) if value.get("code").is_some() && value.get("run").is_none() => {
                Err(value.to_string())
            }
            Ok(value) => Ok(value),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn create_ticket(&self, key: &str, title: &str, extra: Value) -> Value {
        let mut params = json!({
            "requestId": key,
            "projectId": self.project,
            "title": title,
            "description": "Implement `add` in src/lib.rs.\n\n## Acceptance criteria\n- `add` returns the sum of both arguments.\n- Unit checks pass.\n",
            "implementation": {"harness": "codex", "model": IMPLEMENTER_MODEL},
            "review": {"harness": "claude-code", "model": REVIEWER_MODEL},
            "completion": {"mode": "automaticChecksAndReview", "requiredChecks": ["unit"]},
        });
        if let Value::Object(extra) = extra {
            params.as_object_mut().unwrap().extend(extra);
        }
        self.ok("BoardCreate", params).await["item"].clone()
    }

    async fn detail(&self, id: &str) -> Value {
        self.ok("BoardRead", json!({"workItemId": id})).await
    }

    async fn status(&self, id: &str) -> Value {
        self.ok("BoardReviewStatus", json!({"workItemId": id}))
            .await
    }

    async fn working_chat(&self, id: &str) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let detail = self.detail(id).await;
            if let Some(link) = detail["links"]
                .as_array()
                .unwrap()
                .iter()
                .find(|link| link["role"] == "working" && link["active"] == true)
            {
                return link["taskId"].as_str().unwrap().to_owned();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no working chat for {id}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Poll the review status until `done` accepts it.
    async fn wait_status(&self, id: &str, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let status = self.status(id).await;
            if done(&status) {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: timed out waiting for {what}: {status:#}",
                self.evidence.scenario
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn wait_detail(&self, id: &str, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let detail = self.detail(id).await;
            if done(&detail) {
                return detail;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: timed out waiting for {what}: {:#}",
                self.evidence.scenario,
                detail["view"]
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn move_to(&self, id: &str, key: &str, to: &str) -> Value {
        self.ok(
            "BoardMove",
            json!({"requestId": key, "workItemId": id, "to": to}),
        )
        .await
    }

    async fn main_chat(&self, id: &str, title: &str) {
        self.ok(
            "ExecuteTaskCommand",
            json!({"commandId": format!("create-{id}"), "taskId": id, "expectedTaskRevision": 0,
                "intent": {"kind": "createChat", "spaceId": self.project, "cwd": self.repo,
                    "config": {"harness": "codex", "model": IMPLEMENTER_MODEL, "sandbox": "workspace-write", "permissionMode": "ask"}}}),
        )
        .await;
        // Named before any provider session exists, so the title is local
        // (a later ticket conversion keeps it instead of renaming).
        let snapshot = self
            .ok("GetTaskCommandSnapshot", json!({"taskId": id}))
            .await["snapshot"]
            .clone();
        let renamed = self
            .ok(
                "ExecuteTaskCommand",
                json!({"commandId": format!("title-{id}"), "actorId": "board-review-e2e", "clientId": "board-review-e2e",
                    "clientSessionId": "e2e", "taskId": id, "expectedTaskRevision": snapshot["taskRevision"],
                    "intent": {"kind": "rename", "title": title}}),
            )
            .await;
        assert_eq!(renamed["status"], "applied", "{renamed}");
    }

    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.repo.join(path), text).unwrap();
    }

    fn chat(&self, id: &str) -> luna_proto::Chat {
        self.core()
            .workspace
            .chat(id)
            .unwrap()
            .expect("chat exists")
    }

    fn chat_count(&self) -> usize {
        self.core().workspace.read_chats().unwrap().len()
    }

    fn transcript(&self, id: &str) -> Vec<luna_doc::SessionMessageEntry> {
        self.core()
            .doc_host
            .open(id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
    }

    async fn events(&self, id: &str) -> Vec<String> {
        self.ok("BoardEvents", json!({"workItemId": id, "limit": 500}))
            .await["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["kind"]["kind"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn profile_id(&self) -> String {
        self.ok("EngineInfo", json!({})).await["profileId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn open_tab(&self, main: &str, chat: &str) {
        let profile = self.profile_id().await;
        let layout = self
            .ok("GetChatWorkbench", json!({"mainChatId": main}))
            .await;
        let result = self
            .ok(
                "MutateChatWorkbench",
                json!({"mainChatId": main, "profileId": profile, "expectedRevision": layout["revision"],
                    "commandId": format!("qa-open-{chat}"), "command": {"op": "openBackgroundTab", "tab": {"chatId": chat}}}),
            )
            .await;
        assert_eq!(result["conflict"], false, "{result}");
    }

    async fn tab_open(&self, main: &str, chat: &str) -> bool {
        let layout = self
            .ok("GetChatWorkbench", json!({"mainChatId": main}))
            .await;
        serde_json::to_string(&layout["root"])
            .unwrap()
            .contains(&format!("\"chatId\":\"{chat}\""))
    }

    /// The display parent from durable lineage: a side-chat row or a
    /// `ChildThread` record (both are side chats in the unified model).
    fn parent_of(&self, id: &str) -> Option<String> {
        self.core().workspace.side_chat_parent(id).or_else(|| {
            self.core()
                .workspace
                .chat(id)
                .ok()
                .flatten()
                .and_then(|chat| chat.child_thread.map(|child| child.parent_task_id))
        })
    }
}

fn runs(status: &Value) -> Vec<Value> {
    status["runs"].as_array().cloned().unwrap_or_default()
}

/// Newest run first (by creation time).
fn latest_run(status: &Value) -> Value {
    let mut runs = runs(status);
    runs.sort_by_key(|run| std::cmp::Reverse(run["createdAtMs"].as_i64().unwrap_or_default()));
    runs.into_iter().next().unwrap_or(Value::Null)
}

fn review_start(key: &str, item: &str, passed: bool) -> Value {
    json!({
        "requestId": key,
        "workItemId": item,
        "checks": [{"name": "unit", "command": "cargo test", "passed": passed, "exitCode": if passed { 0 } else { 1 }}],
        "summary": "Implemented add; unit checks ran.",
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_initial_result_is_in_the_review_packet() {
    committed_initial_result("committed_initial_result", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_first_result_in_an_unborn_repository_is_reviewed() {
    committed_initial_result("committed_unborn_result", true).await;
}

async fn committed_initial_result(scenario: &'static str, unborn: bool) {
    let mut rig = Rig::new(scenario).await;
    if unborn {
        // Empty history at launch, followed by the worker's first commit.
        git(&rig.repo, &["update-ref", "-d", "refs/heads/main"]);
    }
    let item = rig
        .create_ticket("committed", "Add numbers", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    rig.move_to(&id, "committed-ready", "ready").await;
    let turn = rig
        .ticket_turn(&id, "implementation", |turn| !turn.is_packet())
        .await;
    let worker = rig.working_chat(&id).await;
    rig.write("src/lib.rs", FIXED);
    git(&rig.repo, &["add", "src/lib.rs"]);
    git(&rig.repo, &["commit", "-qm", "implement add"]);
    rig.tool(
        &worker,
        "review_start",
        review_start("committed-review", &id, true),
    )
    .await
    .expect("submit committed result");
    turn.finish();
    let (review, run) = reviewer_turn(&mut rig, &id, "committed review").await;
    rig.evidence.check(
        "the review compares the committed result with the pre-launch revision",
        run["basis"]["baseRevision"] != run["basis"]["headRevision"]
            && review.prompt.contains("+    a + b"),
        run["basis"].clone(),
    );
    let reviewer = run["reviewerTaskId"].as_str().unwrap();
    rig.tool(
        reviewer,
        "review_submit",
        submit("committed-pass", &run, "passed", json!([])),
    )
    .await
    .expect("pass committed result");
    review.finish();
    let detail = rig
        .wait_detail(&id, "committed Done", |d| {
            d["view"]["item"]["column"] == "done"
        })
        .await;
    rig.evidence.step(
        "committed result completes automatically",
        json!("done"),
        detail["view"]["item"]["column"].clone(),
        Some(&detail),
    );
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_repair_checks_match_the_retained_review_base() {
    let mut rig = Rig::new("committed_repair_checks").await;
    let item = rig
        .create_ticket("repair-commit", "Add numbers", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    let (worker, _) = implement_and_submit(&mut rig, &id, "repair-commit-ready", DEFECT).await;
    let (review, run0) = reviewer_turn(&mut rig, &id, "initial review").await;
    let reviewer = run0["reviewerTaskId"].as_str().unwrap().to_owned();
    rig.tool(
        &reviewer,
        "review_submit",
        submit(
            "request-committed-repair",
            &run0,
            "changesRequested",
            defect_finding("subtract", "add subtracts instead of adding."),
        ),
    )
    .await
    .expect("request repair");
    review.finish();
    let repair = rig.turn("repair", Turn::is_repair).await;
    rig.write("src/lib.rs", FIXED);
    git(&rig.repo, &["add", "src/lib.rs"]);
    git(&rig.repo, &["commit", "-qm", "repair add"]);
    rig.tool(
        &worker,
        "review_start",
        review_start("committed-repair-review", &id, true),
    )
    .await
    .expect("submit committed repair");
    repair.finish();
    let (review, run1) = reviewer_turn(&mut rig, &id, "committed repair review").await;
    rig.evidence.check(
        "fresh checks are included for the retained review base",
        run1["basis"]["baseRevision"] == run0["basis"]["baseRevision"]
            && run1["basis"]["checkOutcomeIds"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty())
            && review.prompt.contains("+    a + b"),
        run1["basis"].clone(),
    );
    rig.tool(
        &reviewer,
        "review_submit",
        submit("committed-repair-pass", &run1, "passed", json!([])),
    )
    .await
    .expect("pass repair");
    review.finish();
    let detail = rig
        .wait_detail(&id, "repair Done", |d| {
            d["view"]["item"]["column"] == "done"
        })
        .await;
    rig.evidence.step(
        "committed repair completes automatically",
        json!("done"),
        detail["view"]["item"]["column"].clone(),
        Some(&detail),
    );
    rig.evidence.write();
}

fn submit(key: &str, run: &Value, verdict: &str, findings: Value) -> Value {
    json!({
        "requestId": key,
        "reviewRunId": run["id"],
        "basisPacketHash": run["basis"]["packetHash"],
        "verdict": verdict,
        "summary": format!("Reviewed snapshot {}.", run["basis"]["diffHash"].as_str().unwrap_or_default()),
        "findings": findings,
    })
}

fn defect_finding(id: &str, explanation: &str) -> Value {
    json!([{
        "id": id,
        "severity": "blocker",
        "blocking": true,
        "file": "src/lib.rs",
        "line": 2,
        "explanation": explanation,
        "requiredAction": "Return a + b and keep the unit check passing.",
        "evidence": ["src/lib.rs:2 subtracts"],
    }])
}

/// Start the ticket and let the implementer write `text`, then submit its
/// result for review from its own turn. Returns (working chat, review run).
async fn implement_and_submit(
    rig: &mut Rig,
    item: &str,
    move_key: &str,
    text: &str,
) -> (String, Value) {
    rig.move_to(item, move_key, "ready").await;
    let turn = rig
        .ticket_turn(item, "implementer first start", |turn| {
            turn.harness == HarnessId::Codex && !turn.is_packet()
        })
        .await;
    let worker = rig.working_chat(item).await;
    rig.write("src/lib.rs", text);
    let started = rig
        .tool(
            &worker,
            "review_start",
            review_start(&format!("{move_key}-review"), item, true),
        )
        .await
        .expect("implementer review_start");
    turn.finish();
    (worker, started["run"].clone())
}

/// The reviewer's packet turn and the run it delivers. A packet for a run
/// that is no longer active (superseded after a restart, or re-dispatched
/// by the queue) is finished and skipped.
async fn reviewer_turn(rig: &mut Rig, item: &str, what: &str) -> (Turn, Value) {
    loop {
        let turn = rig
            .turn(what, |turn| {
                turn.is_packet() && !turn.interrupted.load(Ordering::SeqCst)
            })
            .await;
        let deadline = tokio::time::Instant::now() + WAIT;
        let run = loop {
            let status = rig.status(item).await;
            let run = runs(&status).into_iter().find(|run| {
                run["id"]
                    .as_str()
                    .is_some_and(|id| turn.prompt.contains(id))
            });
            match run
                .as_ref()
                .map(|run| run["state"].as_str().unwrap_or_default().to_owned())
            {
                Some(state) if state == "running" || state == "waiting" => break run,
                Some(state)
                    if state == "starting" || state == "outcomeUnknown" || state == "queued" => {}
                _ => break None,
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: {what}: run never became running",
                rig.evidence.scenario
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        };
        let Some(run) = run else {
            turn.finish();
            continue;
        };
        assert!(
            turn.prompt
                .contains(run["basis"]["packetHash"].as_str().unwrap()),
            "packet carries its hash"
        );
        // `review_submit` judges the result by the model the engine observed
        // running the reviewer. A real reviewer submits long after its session
        // started; a scenario submits at once, so it first waits for that
        // observation instead of racing the run loop under load.
        let reviewer = run["reviewerTaskId"].as_str().unwrap().to_owned();
        while rig
            .core()
            .sessions
            .execution_summary(&reviewer)
            .ok()
            .flatten()
            .and_then(|summary| summary.running)
            .and_then(|running| running.model)
            .is_none()
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: {what}: the reviewer's running model was never observed",
                rig.evidence.scenario
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        return (turn, run);
    }
}

// ---------------------------------------------------------------------------
// Scenarios 1, 3, 8: main ticket, seeded defect → repair → re-review → Done,
// exact reviewer archive + closed views, archived details readable.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn main_ticket_codex_reviewed_by_claude_repairs_seeded_defect_then_cleans_up() {
    let mut rig = Rig::new("main_ticket_repair_done_cleanup").await;
    let item = rig.create_ticket("main-1", "Add numbers", json!({})).await;
    let id = item["id"].as_str().unwrap().to_owned();
    let key = item["key"].as_str().unwrap().to_owned();
    let chats_before = rig.chat_count();

    // Implementer (Codex) seeds a real defect and submits from its turn.
    let (worker, queued) = implement_and_submit(&mut rig, &id, "main-1-ready", DEFECT).await;
    rig.evidence.step(
        "implementer review_start records a queued pass",
        json!("queued"),
        queued["state"].clone(),
        Some(&queued),
    );
    let retried = rig
        .tool(
            &worker,
            "review_start",
            review_start("main-1-ready-review", &id, true),
        )
        .await
        .expect("retry returns the stored run");
    rig.evidence.step(
        "retried review_start returns the same run",
        queued["id"].clone(),
        retried["run"]["id"].clone(),
        None,
    );
    let worker_chat = rig.chat(&worker);
    let primary_harness = worker_chat.config.as_ref().map(|config| config.harness);

    // Reviewer (Claude Code) gets a fresh, read-only, bounded packet.
    let (review_turn, run0) = reviewer_turn(&mut rig, &id, "initial review").await;
    let reviewer = run0["reviewerTaskId"].as_str().unwrap().to_owned();
    rig.evidence.step(
        "reviewer harness/model/access",
        json!({"harness": "ClaudeCode", "model": REVIEWER_MODEL, "sandbox": "ReadOnly", "permission": "Ask", "resume": null}),
        json!({
            "harness": format!("{:?}", review_turn.harness),
            "model": review_turn.model,
            "sandbox": format!("{:?}", review_turn.sandbox),
            "permission": format!("{:?}", review_turn.permission_mode),
            "resume": review_turn.resume,
        }),
        None,
    );
    rig.evidence.check(
        "packet carries the seeded defect, criteria, guidance and checks",
        review_turn.prompt.contains("a - b")
            && review_turn.prompt.contains("Acceptance criteria")
            && review_turn.prompt.contains("Keep functions pure")
            && review_turn.prompt.contains("`unit`: passed")
            && review_turn.prompt.contains("review_submit"),
        json!(review_turn.prompt.len()),
    );
    let reviewer_chat = rig.chat(&reviewer);
    rig.evidence.step(
        "reviewer placement: one side chat directly under the main ticket",
        json!({"sideParent": worker, "title": format!("Review · {key} · Add numbers"), "harness": "ClaudeCode", "placement": "underTicket"}),
        json!({
            "sideParent": rig.parent_of(&reviewer),
            "title": reviewer_chat.title,
            "harness": format!("{:?}", reviewer_chat.config.as_ref().unwrap().harness),
            "placement": run0["placement"],
        }),
        None,
    );
    rig.evidence.check(
        "reviewer link role is Reviewer, never Working",
        rig.detail(&id).await["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["taskId"] == reviewer.as_str() && link["role"] == "reviewer"),
        json!(reviewer),
    );
    // A user view of the reviewer, to prove cleanup closes it.
    rig.open_tab(&worker, &reviewer).await;

    // Another chat (the implementer) cannot submit the reviewer's result.
    let forged = rig
        .tool(
            &worker,
            "review_submit",
            submit("forged", &run0, "passed", json!([])),
        )
        .await;
    rig.evidence.check(
        "implementer cannot submit the review result",
        forged.is_err(),
        json!(forged),
    );

    // Blocking finding on the seeded defect.
    let result = rig
        .tool(
            &reviewer,
            "review_submit",
            submit(
                "round-0",
                &run0,
                "changesRequested",
                defect_finding("f1", "add subtracts instead of adding."),
            ),
        )
        .await
        .expect("reviewer submits");
    rig.evidence.step(
        "structured result accepted for the current basis",
        json!({"state": "completed", "verdict": "changesRequested"}),
        json!({"state": result["run"]["state"], "verdict": result["run"]["result"]["verdict"]}),
        Some(&result),
    );
    rig.evidence.step(
        "observed model read back from the running reviewer",
        json!(REVIEWER_MODEL),
        result["run"]["profile"]["observedModel"].clone(),
        None,
    );
    review_turn.finish();

    // Findings return to the implementer once, attributed (not the user).
    let repair = rig
        .turn("repair request", |turn| {
            turn.harness == HarnessId::Codex && turn.is_repair()
        })
        .await;
    rig.evidence.check(
        "repair request is attributed and carries the finding",
        repair.prompt.contains("not a message from the user")
            && repair.prompt.contains("add subtracts instead of adding")
            && repair.prompt.contains(&format!("Reviewer · {key}")),
        json!(repair.prompt.lines().next()),
    );
    rig.evidence.check(
        "result card in the ticket chat",
        rig.transcript(&worker).iter().any(|entry| {
            entry.role == luna_doc::MessageRole::System
                && format!("{:?}", entry.parts).contains("1 blocking finding")
        }),
        json!(worker),
    );
    rig.write("src/lib.rs", FIXED);
    rig.tool(&worker, "review_start", review_start("fix-1", &id, true))
        .await
        .expect("implementer resubmits after repair");
    repair.finish();

    // Re-review in the same reviewer chat sees the final diff.
    let (second, run1) = reviewer_turn(&mut rig, &id, "re-review").await;
    rig.evidence.step(
        "re-review reuses the same reviewer chat, round 1",
        json!({"reviewer": reviewer, "round": 1}),
        json!({"reviewer": run1["reviewerTaskId"], "round": run1["round"]}),
        None,
    );
    rig.evidence.check(
        "re-review packet shows the repaired code",
        second.prompt.contains("a + b") && !second.prompt.contains("a - b\n}"),
        json!(run1["basis"]["diffHash"]),
    );
    rig.tool(
        &reviewer,
        "review_submit",
        submit("round-1", &run1, "passed", json!([])),
    )
    .await
    .expect("reviewer passes");
    second.finish();

    // Done without a human click, then exact cleanup.
    let detail = rig
        .wait_detail(&id, "Done", |detail| {
            detail["view"]["item"]["column"] == "done"
        })
        .await;
    let status = rig
        .wait_status(&id, "cleanup closed", |status| {
            runs(status)
                .iter()
                .any(|run| run["cleanup"]["state"] == "closed")
        })
        .await;
    rig.evidence.step(
        "repair accounting",
        json!({"passes": 2, "repairs": 1}),
        json!({"passes": status["reviewPasses"], "repairs": status["repairRounds"]}),
        Some(&status),
    );
    rig.evidence.step(
        "exactly one reviewer chat was created",
        json!(chats_before + 2),
        json!(rig.chat_count()),
        None,
    );
    rig.evidence.step(
        "reviewer archived; primary unchanged and active",
        json!({"reviewerArchived": true, "primaryArchived": false, "primaryHarness": primary_harness.map(|h| format!("{h:?}"))}),
        json!({
            "reviewerArchived": rig.chat(&reviewer).archived,
            "primaryArchived": rig.chat(&worker).archived,
            "primaryHarness": rig.chat(&worker).config.map(|config| format!("{:?}", config.harness)),
        }),
        None,
    );
    rig.evidence.step(
        "reviewer view closed after archive",
        json!(false),
        json!(rig.tab_open(&worker, &reviewer).await),
        None,
    );
    let events = rig.events(&id).await;
    rig.evidence.check(
        "journal: review started/result/completed/cleaned exactly once",
        events.iter().filter(|kind| *kind == "completed").count() == 1
            && events
                .iter()
                .filter(|kind| *kind == "reviewerCleaned")
                .count()
                == 1
            && events.iter().filter(|kind| *kind == "reviewResult").count() == 2,
        json!(events),
    );
    let outcomes = rig.ok("BoardOutcomes", json!({"workItemId": id})).await;
    let kinds = outcomes["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|outcome| outcome["historical"] != true)
        .map(|outcome| outcome["kind"].as_str().unwrap().to_owned())
        .collect::<HashSet<_>>();
    rig.evidence.check(
        "PB-3 evidence: diff, check and review result",
        kinds.contains("diff") && kinds.contains("check") && kinds.contains("reviewResult"),
        json!(kinds),
    );
    // Archived review details stay readable without unarchiving.
    let transcript = rig.transcript(&reviewer);
    rig.evidence.check(
        "archived reviewer transcript and findings readable, still archived",
        transcript
            .iter()
            .any(|entry| format!("{:?}", entry.parts).contains("LUNA review packet"))
            && detail["reviewRuns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|run| run["result"]["findings"][0]["id"] == "f1")
            && rig.chat(&reviewer).archived,
        json!(transcript.len()),
    );
    let chat = worker.clone();
    let settled = rig
        .wait_detail(&id, "settled and deletable", |detail| {
            detail["view"]["actions"]
                .as_array()
                .is_some_and(|actions| actions.contains(&json!("delete")))
        })
        .await;
    let fence = rig
        .ok("GetTaskCommandSnapshot", json!({"taskId":chat}))
        .await["snapshot"]
        .clone();
    let sent = rig.ok("ExecuteTaskCommand", json!({"commandId":"later-human-work","taskId":chat,"expectedTaskRevision":fence["taskRevision"],"expectedBindingGeneration":fence["bindingGeneration"],"intent":{"kind":"queueMessage","messageId":"later-human-message","text":"A later human request after the ticket was completed."}})).await;
    assert!(
        sent["status"] == "applied" || sent["status"] == "pending",
        "later human send was refused: {sent}"
    );
    let later = rig
        .turn("later human work", |turn| {
            turn.prompt.contains("A later human request")
        })
        .await;
    let refused = rig.call("BoardDelete", json!({"requestId":"delete-completed-ticket","workItemId":id,"expectedRevision":settled["view"]["item"]["revision"]})).await;
    assert!(
        refused.is_err(),
        "Done must not hide a live worker from deletion admission: {refused:?}"
    );
    later.finish();
    rig.wait_idle(&chat).await;
    let settled = rig
        .wait_detail(&id, "deletable after later work ends", |detail| {
            detail["view"]["actions"]
                .as_array()
                .is_some_and(|actions| actions.contains(&json!("delete")))
        })
        .await;
    let deleted = rig.ok("BoardDelete", json!({"requestId":"delete-completed-ticket","workItemId":id,"expectedRevision":settled["view"]["item"]["revision"]})).await;
    rig.evidence.step("settled deletion keeps the later chat and its history", json!({"deleted":true,"archived":false,"history":true}), json!({"deleted":deleted["item"]["deleted"],"archived":rig.chat(&chat).archived,"history":rig.transcript(&chat).iter().any(|entry|entry.id=="later-human-message")}), Some(&deleted));
    rig.evidence.write();
}

// ---------------------------------------------------------------------------
// Scenarios 2 and 10: side ticket → fresh sibling reviewer under the same
// main; Stop reaches exactly the owned executions; a late pass is history.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn side_ticket_sibling_reviewer_and_stop_during_review() {
    let mut rig = Rig::new("side_ticket_sibling_stop").await;
    let main = uuid::Uuid::new_v4().to_string();
    rig.main_chat(&main, "Planning").await;
    let sibling = rig
        .ok(
            "BoardCreateSideChat",
            json!({"requestId": "unrelated-sibling", "callerTaskId": main, "title": "Unrelated sibling", "start": false}),
        )
        .await["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let item = rig.create_ticket("side-1", "Side add", json!({})).await;
    let id = item["id"].as_str().unwrap().to_owned();
    rig.ok(
        "BoardStart",
        json!({"requestId": "side-1-start", "workItemId": id, "destination": {"kind": "sideOf", "mainTaskId": main}, "callerTaskId": main}),
    )
    .await;
    let turn = rig
        .turn("side implementer", |turn| {
            turn.harness == HarnessId::Codex && !turn.is_packet()
        })
        .await;
    let worker = rig.working_chat(&id).await;
    rig.evidence.step(
        "ticket worker is a side chat of the main",
        json!(main),
        json!(rig.parent_of(&worker)),
        None,
    );
    rig.write("src/lib.rs", FIXED);
    rig.tool(
        &worker,
        "review_start",
        review_start("side-review", &id, true),
    )
    .await
    .expect("side worker submits");
    turn.finish();

    let (review_turn, run) = reviewer_turn(&mut rig, &id, "sibling review").await;
    let reviewer = run["reviewerTaskId"].as_str().unwrap().to_owned();
    rig.evidence.step(
        "reviewer is a fresh sibling under the same main, owned by the ticket",
        json!({"parent": main, "placement": "siblingUnderMain", "display": main, "primary": worker, "resume": null}),
        json!({
            "parent": rig.parent_of(&reviewer),
            "placement": run["placement"],
            "display": run["displayMainParentId"],
            "primary": run["primaryTaskId"],
            "resume": review_turn.resume,
        }),
        Some(&run),
    );
    rig.evidence.check(
        "no nested side chat under the side ticket",
        rig.core()
            .workspace
            .read_chats()
            .unwrap()
            .iter()
            .all(|chat| rig.parent_of(&chat.id).as_deref() != Some(worker.as_str())),
        json!(worker),
    );

    // Stop: Ready → Backlog during the sibling review.
    rig.move_to(&id, "side-1-stop", "backlog").await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !review_turn.interrupted.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "reviewer was not stopped"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let status = rig
        .wait_status(&id, "cancelled run", |status| {
            runs(status)
                .iter()
                .any(|r| r["id"] == run["id"] && r["state"] == "cancelled")
        })
        .await;
    rig.evidence.step(
        "shared Stop reached the exact sibling reviewer; run cancelled",
        json!({"interrupted": true, "state": "cancelled"}),
        json!({"interrupted": review_turn.interrupted.load(Ordering::SeqCst),
            "state": runs(&status).iter().find(|r| r["id"] == run["id"]).unwrap()["state"]}),
        Some(&status),
    );
    rig.evidence.step(
        "main and unrelated sibling untouched",
        json!({"mainArchived": false, "siblingArchived": false, "siblingParent": main}),
        json!({"mainArchived": rig.chat(&main).archived, "siblingArchived": rig.chat(&sibling).archived,
            "siblingParent": rig.parent_of(&sibling)}),
        None,
    );
    // A late passing result is history only.
    let late = rig
        .tool(
            &reviewer,
            "review_submit",
            submit("late-pass", &run, "passed", json!([])),
        )
        .await;
    let column = rig.detail(&id).await["view"]["item"]["column"].clone();
    rig.evidence.step(
        "late pass after Stop cannot mark Done",
        json!({"column": "backlog", "lateResult": true}),
        json!({"column": column, "lateResult": late.as_ref().map(|value| value["run"]["lateResult"].clone()).unwrap_or(json!(true))}),
        None,
    );
    review_turn.finish();
    rig.evidence.write();
}

// ---------------------------------------------------------------------------
// Scenario 4: the diff changes during review.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diff_mutated_during_review_is_stale_and_reviewed_again() {
    let mut rig = Rig::new("stale_diff").await;
    let item = rig.create_ticket("stale-1", "Stale add", json!({})).await;
    let id = item["id"].as_str().unwrap().to_owned();
    implement_and_submit(&mut rig, &id, "stale-1-ready", FIXED).await;
    let (turn, run) = reviewer_turn(&mut rig, &id, "first review").await;
    // Files change while the reviewer works.
    rig.write(
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 {\n    a.wrapping_add(b)\n}\n",
    );
    let result = rig
        .tool(
            &turn_owner(&run),
            "review_submit",
            submit("stale-pass", &run, "passed", json!([])),
        )
        .await
        .expect("submit accepted as data");
    rig.evidence.step(
        "old verdict is stale",
        json!("stale"),
        result["run"]["state"].clone(),
        Some(&result),
    );
    rig.evidence.step(
        "ticket not Done",
        json!("ready"),
        rig.detail(&id).await["view"]["item"]["column"].clone(),
        None,
    );
    turn.finish();
    let (again, rerun) = reviewer_turn(&mut rig, &id, "re-review of the new basis").await;
    rig.evidence.check(
        "new basis reviewed in the same reviewer chat",
        rerun["basis"]["diffHash"] != run["basis"]["diffHash"]
            && rerun["reviewerTaskId"] == run["reviewerTaskId"]
            && again.prompt.contains("wrapping_add"),
        json!({"old": run["basis"]["diffHash"], "new": rerun["basis"]["diffHash"]}),
    );
    rig.tool(
        &turn_owner(&rerun),
        "review_submit",
        submit("fresh-pass", &rerun, "passed", json!([])),
    )
    .await
    .expect("pass on the new basis");
    again.finish();
    // The check was reported for the original snapshot, so a required check
    // for the new snapshot is missing: never Done on stale checks.
    let status = rig
        .wait_status(&id, "outcome applied", |status| {
            runs(status)
                .iter()
                .any(|r| r["id"] == rerun["id"] && !r["outcome"].is_null())
        })
        .await;
    let applied = runs(&status)
        .into_iter()
        .find(|r| r["id"] == rerun["id"])
        .unwrap();
    rig.evidence.step(
        "a check from an earlier snapshot does not count",
        json!({"outcome": "needsAttention", "column": "ready"}),
        json!({"outcome": applied["outcome"]["kind"], "column": rig.detail(&id).await["view"]["item"]["column"]}),
        Some(&applied),
    );
    rig.evidence.write();
}

fn turn_owner(run: &Value) -> String {
    run["reviewerTaskId"].as_str().unwrap().to_owned()
}

// ---------------------------------------------------------------------------
// Scenario 5: lost responses and restarts at launch and delivery boundaries.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_at_launch_and_delivery_boundaries_is_once_only() {
    let mut rig = Rig::new("restart_once_only").await;
    let item = rig
        .create_ticket("restart-1", "Restart add", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    rig.move_to(&id, "restart-1-ready", "ready").await;
    let turn = rig
        .turn("implementer", |turn| {
            turn.harness == HarnessId::Codex && !turn.is_packet()
        })
        .await;
    let worker = rig.working_chat(&id).await;
    rig.write("src/lib.rs", DEFECT);
    // A lost response: the same key retried, also concurrently.
    let (a, b) = tokio::join!(
        rig.tool(
            &worker,
            "review_start",
            review_start("restart-review", &id, true)
        ),
        rig.tool(
            &worker,
            "review_start",
            review_start("restart-review", &id, true)
        ),
    );
    let (a, b) = (a.expect("first"), b.expect("second"));
    rig.evidence.step(
        "concurrent retries share one run",
        a["run"]["id"].clone(),
        b["run"]["id"].clone(),
        None,
    );
    // Restart before launch (the implementer turn is still in flight): the
    // queued pass survives with its owner. A provider-side recovery of the
    // interrupted implementer turn is simply allowed to finish.
    rig.restart().await;
    drop(turn);
    rig.autofinish =
        Some(|turn| turn.harness == HarnessId::Codex && !turn.is_packet() && !turn.is_repair());
    let (review_turn, run) = reviewer_turn(&mut rig, &id, "review after restart").await;
    rig.evidence.step(
        "owner association survives restart",
        json!({"primary": worker, "id": a["run"]["id"]}),
        json!({"primary": run["primaryTaskId"], "id": run["id"]}),
        None,
    );
    let reviewer = turn_owner(&run);
    // Restart while the reviewer turn runs (launch boundary): the same
    // reviewer chat continues the pass (relaunch or queue recovery), never a
    // second reviewer chat.
    rig.restart().await;
    drop(review_turn);
    let (relaunched, rerun) = reviewer_turn(&mut rig, &id, "review after launch restart").await;
    let reviewer_links = rig.detail(&id).await["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|link| link["role"] == "reviewer")
        .count();
    rig.evidence.step(
        "one reviewer chat across restarts",
        json!({"reviewer": reviewer, "links": 1, "sameChain": true}),
        json!({"reviewer": rerun["reviewerTaskId"], "links": reviewer_links,
            "sameChain": rerun["id"] == run["id"] || rerun["supersedes"] == run["id"]}),
        Some(&rerun),
    );
    // Delivery boundary: the repair request is delivered, then the engine
    // restarts while the repair turn runs; nothing is delivered again.
    rig.tool(
        &reviewer,
        "review_submit",
        submit(
            "restart-findings",
            &rerun,
            "changesRequested",
            defect_finding("f1", "add subtracts."),
        ),
    )
    .await
    .expect("findings submitted");
    let repair = rig
        .turn("repair request", |turn| {
            turn.harness == HarnessId::Codex && turn.is_repair()
        })
        .await;
    rig.restart().await;
    drop(repair);
    drop(relaunched);
    let repair_id = format!("{}:repair", rerun["id"].as_str().unwrap());
    tokio::time::sleep(Duration::from_secs(3)).await;
    let copies = rig
        .transcript(&worker)
        .iter()
        .filter(|entry| entry.id == repair_id)
        .count();
    let status = rig.status(&id).await;
    let delivered = runs(&status)
        .into_iter()
        .find(|r| r["id"] == rerun["id"])
        .unwrap();
    rig.evidence.step(
        "repair request delivered exactly once across the restart",
        json!({"copies": 1, "deliveredId": repair_id, "repairs": 1}),
        json!({"copies": copies, "deliveredId": delivered["deliveredId"], "repairs": status["repairRounds"]}),
        Some(&status),
    );
    rig.evidence.write();
}

// ---------------------------------------------------------------------------
// Scenario 6: unresolved states never complete.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_reviewer_model_is_unresolved_without_fallback() {
    let mut rig = Rig::new("unavailable_reviewer").await;
    let item = rig
        .create_ticket("unavailable-1", "Unavailable reviewer", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    rig.move_to(&id, "unavailable-1-ready", "ready").await;
    let turn = rig
        .turn("implementer", |turn| turn.harness == HarnessId::Codex)
        .await;
    let worker = rig.working_chat(&id).await;
    rig.write("src/lib.rs", FIXED);
    // An explicit reviewer selection the target account cannot run.
    let mut request = review_start("unavailable-review", &id, true);
    request["profile"] = json!({"harness": "claude-code", "model": "opus-9-unreleased"});
    let refused = rig.tool(&worker, "review_start", request).await;
    turn.finish();
    rig.evidence.check(
        "structured unavailable-model error with supported choices",
        refused.as_ref().err().is_some_and(|error| {
            error.contains("unavailableModel") && error.contains(REVIEWER_MODEL)
        }),
        json!(refused),
    );
    let status = rig.status(&id).await;
    let run = latest_run(&status);
    rig.evidence.step(
        "run recorded as failed with model attention; no substitute",
        json!({"state": "failed", "attention": "modelUnavailable", "reviewer": null, "model": "opus-9-unreleased"}),
        json!({"state": run["state"], "attention": run["attention"], "reviewer": run["reviewerTaskId"],
            "model": run["profile"]["resolved"]["model"]}),
        Some(&status),
    );
    let quiet = rig
        .no_turn(Duration::from_secs(3), |turn| turn.is_packet())
        .await;
    rig.evidence
        .check("no reviewer turn ran", quiet, json!(null));
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn omitted_checks_and_unobserved_substitution_never_complete() {
    let mut rig = Rig::new("omitted_checks_substitution").await;
    // Omitted required check: the review passes, Done does not happen.
    let item = rig
        .create_ticket("omitted-1", "Omitted checks", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    rig.move_to(&id, "omitted-1-ready", "ready").await;
    let turn = rig
        .turn("implementer", |turn| turn.harness == HarnessId::Codex)
        .await;
    let worker = rig.working_chat(&id).await;
    rig.write("src/lib.rs", FIXED);
    rig.tool(
        &worker,
        "review_start",
        json!({"requestId": "omitted-review", "workItemId": id, "summary": "No checks run."}),
    )
    .await
    .expect("review without checks still runs");
    turn.finish();
    let (review, run) = reviewer_turn(&mut rig, &id, "review without checks").await;
    rig.evidence.check(
        "packet declares the missing required check",
        review
            .prompt
            .contains("Required check `unit` has no result"),
        json!(run["basis"]["omissions"]),
    );
    rig.tool(
        &turn_owner(&run),
        "review_submit",
        submit("omitted-pass", &run, "passed", json!([])),
    )
    .await
    .expect("pass");
    review.finish();
    let status = rig
        .wait_status(&id, "outcome", |status| {
            !latest_run(status)["outcome"].is_null()
        })
        .await;
    rig.evidence.step(
        "skipped required check never yields Done",
        json!({"outcome": "needsAttention", "column": "ready"}),
        json!({"outcome": latest_run(&status)["outcome"]["kind"], "column": rig.detail(&id).await["view"]["item"]["column"]}),
        Some(&status),
    );

    // Unobserved substitution: the provider reports another model.
    let detail = rig
        .wait_detail(&id, "idle after missing checks", |d| {
            d["view"]["phase"] == "idle"
        })
        .await;
    rig.evidence.check(
        "missing required checks show attention and Retry on the Board",
        detail["view"]["attention"]
            .as_array()
            .is_some_and(|reasons| {
                reasons.iter().any(|r| {
                    r["detail"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Required checks")
                })
            })
            && detail["view"]["actions"]
                .as_array()
                .is_some_and(|actions| actions.contains(&json!("retry"))),
        detail["view"].clone(),
    );
    *rig.control.reviewer_reports.lock().unwrap() = Some("sonnet-5".into());
    let item = rig
        .create_ticket("subst-1", "Substitution", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    rig.move_to(&id, "subst-1-ready", "ready").await;
    let turn = rig
        .turn("implementer", |turn| {
            turn.harness == HarnessId::Codex && !turn.is_packet()
        })
        .await;
    let worker = rig.working_chat(&id).await;
    rig.write(
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 {\n    b + a\n}\n",
    );
    rig.tool(
        &worker,
        "review_start",
        review_start("subst-review", &id, true),
    )
    .await
    .expect("submitted");
    turn.finish();
    let (review, run) = reviewer_turn(&mut rig, &id, "substituted review").await;
    let result = rig
        .tool(
            &turn_owner(&run),
            "review_submit",
            submit("subst-pass", &run, "passed", json!([])),
        )
        .await
        .expect("result recorded");
    review.finish();
    rig.evidence.step(
        "observed substitution is unresolved",
        json!({"state": "failed", "attention": "modelUnavailable", "observed": "sonnet-5", "column": "ready"}),
        json!({"state": result["run"]["state"], "attention": result["run"]["attention"],
            "observed": result["run"]["profile"]["observedModel"], "column": rig.detail(&id).await["view"]["item"]["column"]}),
        Some(&result),
    );
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_prose_without_result_is_relaunched_once_then_unresolved() {
    let mut rig = Rig::new("prose_without_result").await;
    let item = rig
        .create_ticket("prose-1", "Prose reviewer", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    implement_and_submit(&mut rig, &id, "prose-1-ready", FIXED).await;
    let (first, run) = reviewer_turn(&mut rig, &id, "first review").await;
    first.finish(); // "Looks good" prose; no review_submit.
    let (second, rerun) = reviewer_turn(&mut rig, &id, "relaunch").await;
    rig.evidence.step(
        "one automatic relaunch into the same reviewer",
        json!({"supersedes": run["id"], "reviewer": run["reviewerTaskId"]}),
        json!({"supersedes": rerun["supersedes"], "reviewer": rerun["reviewerTaskId"]}),
        None,
    );
    second.finish();
    let status = rig
        .wait_status(&id, "unresolved", |status| {
            runs(status)
                .iter()
                .any(|r| r["id"] == rerun["id"] && r["state"] == "failed")
        })
        .await;
    let failed = runs(&status)
        .into_iter()
        .find(|r| r["id"] == rerun["id"])
        .unwrap();
    rig.evidence.step(
        "provider exit without review_submit is unresolved",
        json!({"attention": "reviewUnavailable", "column": "ready"}),
        json!({"attention": failed["attention"], "column": rig.detail(&id).await["view"]["item"]["column"]}),
        Some(&failed),
    );
    let quiet = rig
        .no_turn(Duration::from_secs(3), |turn| turn.is_packet())
        .await;
    rig.evidence
        .check("no third reviewer turn", quiet, json!(null));
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_repair_rounds_need_attention() {
    let mut rig = Rig::new("repair_cap").await;
    let item = rig.create_ticket("cap-1", "Repair cap", json!({})).await;
    let id = item["id"].as_str().unwrap().to_owned();
    let (worker, _) = implement_and_submit(&mut rig, &id, "cap-1-ready", DEFECT).await;
    let variants = [
        "pub fn add(a: i32, b: i32) -> i32 {\n    a * b\n}\n",
        "pub fn add(a: i32, b: i32) -> i32 {\n    a / b\n}\n",
    ];
    for pass in 0..3 {
        let (review, run) = reviewer_turn(&mut rig, &id, &format!("pass {pass}")).await;
        rig.tool(
            &turn_owner(&run),
            "review_submit",
            submit(
                &format!("cap-{pass}"),
                &run,
                "changesRequested",
                defect_finding(
                    &format!("f{pass}"),
                    &format!("Defect variant {pass} is still wrong."),
                ),
            ),
        )
        .await
        .expect("findings");
        review.finish();
        // Two repairs (one per variant), then the cap.
        if let Some(variant) = variants.get(pass) {
            let repair = rig
                .turn("repair", |turn| {
                    turn.harness == HarnessId::Codex && turn.is_repair()
                })
                .await;
            rig.write("src/lib.rs", variant);
            rig.tool(
                &worker,
                "review_start",
                review_start(&format!("cap-fix-{pass}"), &id, true),
            )
            .await
            .expect("resubmit");
            repair.finish();
        }
    }
    let status = rig
        .wait_status(&id, "cap reached", |status| {
            latest_run(status)["outcome"]["kind"] == "needsAttention"
        })
        .await;
    rig.evidence.step(
        "three passes, two repairs, then Needs attention",
        json!({"passes": 3, "repairs": 2, "attention": "reviewBlocked", "column": "ready"}),
        json!({"passes": status["reviewPasses"], "repairs": status["repairRounds"],
            "attention": latest_run(&status)["attention"], "column": rig.detail(&id).await["view"]["item"]["column"]}),
        Some(&status),
    );
    let quiet = rig
        .no_turn(Duration::from_secs(3), |turn| turn.is_repair())
        .await;
    rig.evidence
        .check("no third repair request", quiet, json!(null));
    let more = rig
        .tool(&worker, "review_start", review_start("cap-more", &id, true))
        .await;
    rig.evidence.check(
        "a model-driven retry cannot grant itself another round",
        more.is_err(),
        json!(more),
    );
    rig.evidence.write();
}

// ---------------------------------------------------------------------------
// A fresh failed check supersedes an old pass on an unchanged diff.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_current_failed_check_cannot_reuse_an_older_pass() {
    let mut rig = Rig::new("current_failed_check").await;
    let item = rig
        .create_ticket("fresh-check", "Add numbers", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    let (worker, _) = implement_and_submit(&mut rig, &id, "fresh-check-ready", DEFECT).await;
    let (review, run0) = reviewer_turn(&mut rig, &id, "initial review").await;
    let reviewer = run0["reviewerTaskId"].as_str().unwrap().to_owned();
    rig.tool(
        &reviewer,
        "review_submit",
        submit(
            "fresh-check-repair",
            &run0,
            "changesRequested",
            defect_finding("bad", "add subtracts."),
        ),
    )
    .await
    .expect("request repair");
    review.finish();
    let repair = rig.turn("repair", Turn::is_repair).await;
    // The implementation did not change, but its latest test run failed.
    rig.tool(
        &worker,
        "review_start",
        review_start("fresh-check-failed", &id, false),
    )
    .await
    .expect("report failed check");
    repair.finish();
    let (review, run1) = reviewer_turn(&mut rig, &id, "re-review").await;
    rig.tool(
        &reviewer,
        "review_submit",
        submit("fresh-check-verdict", &run1, "passed", json!([])),
    )
    .await
    .expect("review verdict accepted");
    review.finish();
    let status = rig
        .wait_status(&id, "failed check outcome", |s| {
            !latest_run(s)["outcome"].is_null()
        })
        .await;
    rig.evidence.step(
        "latest failed check blocks Done even on the same diff",
        json!("needsAttention"),
        latest_run(&status)["outcome"]["kind"].clone(),
        Some(&status),
    );
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inherited_reviewer_model_substitution_is_rejected() {
    let mut rig = Rig::new("inherited_reviewer_substitution").await;
    *rig.control.inherited_reviewer_reports.lock().unwrap() = Some("different-model".into());
    let item = rig
        .create_ticket("inherit-model", "Add numbers", json!({"review":{}}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    implement_and_submit(&mut rig, &id, "inherit-model-ready", FIXED).await;
    let (review, run) = reviewer_turn(&mut rig, &id, "inherited model review").await;
    let reviewer = run["reviewerTaskId"].as_str().unwrap();
    let result = rig
        .tool(
            reviewer,
            "review_submit",
            submit("inherit-model-pass", &run, "passed", json!([])),
        )
        .await
        .expect("record rejected verdict");
    review.finish();
    rig.evidence.step(
        "inherited implementation model requires matching observation",
        json!({"state":"failed","attention":"modelUnavailable"}),
        json!({"state":result["run"]["state"],"attention":result["run"]["attention"]}),
        Some(&result),
    );
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_artifact_tail_changes_invalidate_review() {
    use std::io::{Seek as _, Write as _};
    let mut rig = Rig::new("large_artifact_tail_change").await;
    let artifact = rig.repo.join("artifact.bin");
    std::fs::write(&artifact, vec![0u8; 17 * 1024 * 1024]).unwrap();
    let item = rig
        .create_ticket("large-artifact", "Add numbers", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    implement_and_submit(&mut rig, &id, "large-artifact-ready", FIXED).await;
    let (review, run) = reviewer_turn(&mut rig, &id, "large artifact review").await;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&artifact)
        .unwrap();
    file.seek(std::io::SeekFrom::Start(2 * 1024 * 1024))
        .unwrap();
    file.write_all(&[1]).unwrap();
    file.sync_all().unwrap();
    let reviewer = run["reviewerTaskId"].as_str().unwrap();
    let result = rig
        .tool(
            reviewer,
            "review_submit",
            submit("large-artifact-pass", &run, "passed", json!([])),
        )
        .await
        .expect("record stale result");
    review.finish();
    rig.evidence.step(
        "a same-size change after the first MiB invalidates review",
        json!("stale"),
        result["run"]["state"].clone(),
        Some(&result),
    );
    let (again, rerun) = reviewer_turn(&mut rig, &id, "review changed binary").await;
    rig.tool(
        &turn_owner(&rerun),
        "review_submit",
        submit("large-artifact-fresh-pass", &rerun, "passed", json!([])),
    )
    .await
    .expect("record fresh review");
    again.finish();
    let status = rig
        .wait_status(&id, "binary check freshness", |status| {
            runs(status)
                .iter()
                .any(|r| r["id"] == rerun["id"] && !r["outcome"].is_null())
        })
        .await;
    let applied = runs(&status)
        .into_iter()
        .find(|r| r["id"] == rerun["id"])
        .unwrap();
    rig.evidence.step(
        "checks for the old binary cannot complete the changed artifact",
        json!("needsAttention"),
        applied["outcome"]["kind"].clone(),
        Some(&applied),
    );
    rig.evidence.write();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_nested_files_are_in_the_review_packet() {
    let mut rig = Rig::new("new_nested_files").await;
    let item = rig
        .create_ticket("nested-files", "Add numbers", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    let nested = rig.repo.join("new-module/nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("new.rs"),
        "pub fn nested_result() -> u32 { 42 }\n",
    )
    .unwrap();
    implement_and_submit(&mut rig, &id, "nested-files-ready", FIXED).await;
    let (review, run) = reviewer_turn(&mut rig, &id, "new nested file review").await;
    rig.evidence.check(
        "new files in untracked directories are visible to the reviewer",
        review.prompt.contains("new-module/nested/new.rs")
            && review.prompt.contains("+pub fn nested_result()"),
        run["basis"].clone(),
    );
    rig.tool(
        &turn_owner(&run),
        "review_submit",
        submit("nested-files-pass", &run, "passed", json!([])),
    )
    .await
    .unwrap();
    review.finish();
    rig.evidence.write();
}

// Scenario 7: converted main chat, implementation-model fallback reviewer.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn converted_main_chat_keeps_identity_with_fallback_reviewer() {
    let mut rig = Rig::new("converted_main_fallback").await;
    let chat = uuid::Uuid::new_v4().to_string();
    rig.main_chat(&chat, "Discussion").await;
    rig.ok(
        "ExecuteTaskCommand",
        json!({"commandId": "seed-history", "taskId": chat, "intent": {"kind": "queueMessage", "messageId": "seed-message", "text": "Original discussion: add needs implementing."}}),
    )
    .await;
    rig.turn("original discussion", |turn| {
        turn.prompt.contains("Original discussion")
    })
    .await
    .finish();
    // Convert an idle chat: a still-live turn is reused in place, not continued.
    rig.wait_idle(&chat).await;
    let converted = rig
        .ok(
            "BoardConvertChat",
            json!({"requestId": "convert-1", "taskId": chat, "title": "Discussion", "description": "Implement add.\n\n## Acceptance criteria\n- returns the sum\n",
                "completion": {"mode": "automaticChecksAndReview", "requiredChecks": ["unit"]}}),
        )
        .await;
    let id = converted["mutation"]["workItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.evidence.step(
        "conversion keeps the chat as its own worker",
        json!(chat),
        json!(rig.working_chat(&id).await),
        Some(&converted),
    );
    rig.move_to(&id, "convert-1-ready", "ready").await;
    let turn = rig
        .ticket_turn(&id, "continuation", |turn| {
            turn.harness == HarnessId::Codex && !turn.is_packet()
        })
        .await;
    rig.write("src/lib.rs", FIXED);
    rig.tool(
        &chat,
        "review_start",
        review_start("convert-review", &id, true),
    )
    .await
    .expect("submitted");
    turn.finish();
    // No reviewer configured: the implementation model in a separate chat.
    let (review, run) = reviewer_turn(&mut rig, &id, "fallback review").await;
    let reviewer = turn_owner(&run);
    rig.evidence.step(
        "fallback reviewer: implementation model, separate side chat under the converted chat",
        json!({"fallback": true, "harness": "Codex", "model": IMPLEMENTER_MODEL, "parent": chat, "separate": true}),
        json!({"fallback": run["profile"]["implementationFallback"], "harness": format!("{:?}", review.harness),
            "model": review.model, "parent": rig.parent_of(&reviewer), "separate": reviewer != chat}),
        Some(&run),
    );
    rig.tool(
        &reviewer,
        "review_submit",
        submit("convert-pass", &run, "passed", json!([])),
    )
    .await
    .expect("pass");
    review.finish();
    rig.wait_detail(&id, "Done", |detail| {
        detail["view"]["item"]["column"] == "done"
    })
    .await;
    rig.evidence.step(
        "identity and history preserved",
        json!({"archived": false, "history": true}),
        json!({"archived": rig.chat(&chat).archived,
            "history": rig.transcript(&chat).iter().any(|entry| entry.id == "seed-message")}),
        None,
    );
    rig.evidence.write();
}

// ---------------------------------------------------------------------------
// Scenario 9: a late reviewer turn and a layout revision conflict leave
// cleanup truthfully pending; retries are idempotent.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_waits_for_late_reviewer_and_survives_layout_conflicts() {
    let mut rig = Rig::new("cleanup_partial_retry").await;
    let item = rig
        .create_ticket("cleanup-1", "Cleanup add", json!({}))
        .await;
    let id = item["id"].as_str().unwrap().to_owned();
    let (worker, _) = implement_and_submit(&mut rig, &id, "cleanup-1-ready", FIXED).await;
    let (review, run) = reviewer_turn(&mut rig, &id, "review").await;
    let reviewer = turn_owner(&run);
    rig.open_tab(&worker, &reviewer).await;
    rig.tool(
        &reviewer,
        "review_submit",
        submit("cleanup-pass", &run, "passed", json!([])),
    )
    .await
    .expect("pass");
    // The reviewer's turn is still running: Done, cleanup pending.
    rig.wait_detail(&id, "Done", |detail| {
        detail["view"]["item"]["column"] == "done"
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let pending = rig.status(&id).await;
    rig.evidence.step(
        "Done · cleanup pending while the reviewer finishes",
        json!({"cleanup": "pending", "archived": false}),
        json!({"cleanup": latest_run(&pending)["cleanup"]["state"], "archived": rig.chat(&reviewer).archived}),
        Some(&pending),
    );
    // Layout changes underneath (a revision conflict for a stale writer).
    let other = uuid::Uuid::new_v4().to_string();
    rig.main_chat(&other, "Unrelated main").await;
    rig.open_tab(&worker, &other).await;
    review.finish();
    let status = rig
        .wait_status(&id, "closed", |status| {
            latest_run(status)["cleanup"]["state"] == "closed"
        })
        .await;
    rig.evidence.step(
        "cleanup completes once the reviewer is idle; unrelated view kept",
        json!({"archived": true, "reviewerTab": false, "otherTab": true, "otherArchived": false}),
        json!({"archived": rig.chat(&reviewer).archived, "reviewerTab": rig.tab_open(&worker, &reviewer).await,
            "otherTab": rig.tab_open(&worker, &other).await, "otherArchived": rig.chat(&other).archived}),
        Some(&status),
    );
    let events = rig.events(&id).await;
    rig.evidence.check(
        "cleanup success journaled once; no duplicate review",
        events
            .iter()
            .filter(|kind| *kind == "reviewerCleaned")
            .count()
            == 1
            && events
                .iter()
                .filter(|kind| *kind == "reviewStarted")
                .count()
                == 1,
        json!(events),
    );
    let quiet = rig
        .no_turn(Duration::from_secs(3), |turn| turn.is_packet())
        .await;
    rig.evidence.check(
        "no further reviewer turn (no paid re-review)",
        quiet,
        json!(null),
    );
    rig.evidence.write();
}
