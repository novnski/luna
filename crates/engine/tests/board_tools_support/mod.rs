//! Shared provider-parity scenario for the board tools E2E (A4). Each
//! provider test (`board_tools_e2e.rs` for Claude Code,
//! `board_tools_codex_e2e.rs`, `board_tools_opencode_e2e.rs`) boots a real
//! engine with its deterministic fake CLI, which is the MCP client of the
//! run's private `luna` endpoint, and runs the same plans through the real
//! tool path: provider fence → engine executor → `CrossTaskTools` → board
//! service. The failure cases are listed in `board_tools_e2e.rs`.
//!
//! A slice that has not landed answers `notImplemented`; the scenario then
//! proves nothing happened (no chat, no column change) instead of skipping.
//! On the merged tree the same steps assert the receipts and placement.
//!
//! Evidence: one JSON file per provider under `target/qa/board/tools/` with
//! every step's expected/observed values and SHA-256 hashes of the raw
//! provider-side results and delivered instructions.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use luna_engine::cross_task_tools::{CrossTaskAuthority, CrossTaskBackend, CrossTaskTools};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, EngineRpc, HarnessRegistry};
use luna_harness::Harness;
use luna_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, PermissionMode, RunRequest, RuntimeDelivery,
    SandboxLevel, TaskLineage,
};
use luna_rpc::RpcClient;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Every board tool an ordinary main chat gets.
pub const MAIN_TOOLS: &[&str] = &[
    "board_list",
    "board_read",
    "board_execution_options",
    "board_create",
    "board_update",
    "board_move",
    "board_start",
    "board_control",
    "board_convert_chat",
    "create_side_chat",
    "create_main_chat",
    "board_issue_search",
    "board_issue_import",
];
/// A ticket's working chat: no conversion, plus status reports and review.
pub const WORKER_TOOLS: &[&str] = &[
    "board_list",
    "board_read",
    "board_execution_options",
    "board_create",
    "board_update",
    "board_move",
    "board_start",
    "board_control",
    "board_set_status",
    "review_start",
    "review_status",
    "review_cancel",
    "create_side_chat",
    "create_main_chat",
    "board_issue_search",
    "board_issue_import",
];
/// The board tools that change nothing.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "board_list",
    "board_read",
    "board_execution_options",
    "review_status",
    "board_issue_search",
];
pub const REVIEWER_TOOLS: &[&str] = &["board_read", "review_status", "review_submit"];
pub const AUTHORING_TOOLS: &[&str] = &[
    "board_list",
    "board_read",
    "board_execution_options",
    "board_create",
    "board_start",
    "board_draft_checkpoint",
];
/// Serialized board tools (`name`, `description`, `inputSchema`, as MCP
/// lists them) an ordinary main or side chat may carry: about 4k tokens.
pub const CATALOG_BUDGET_BYTES: usize = 16 * 1024;
const NOT_ATTACHED: &str = "not attached";

/// How one provider's fake CLI reads plans and reports results.
#[derive(Clone, Copy)]
pub struct Provider {
    pub name: &'static str,
    pub harness: HarnessId,
    pub delivery: RuntimeDelivery,
    /// `<prefix><label>.json` in the run's cwd.
    pub plan_prefix: &'static str,
    pub results_prefix: &'static str,
    /// A plain turn's instruction capture: `<prefix><task>.txt`.
    pub turn_prefix: &'static str,
    /// The prompt that selects a plan: `<prompt><label>`.
    pub plan_prompt: &'static str,
    /// The results key holding the delivered LUNA instructions.
    pub instructions_key: &'static str,
    /// A mid-turn provider compaction, as a plan item.
    pub compaction: fn() -> Value,
}

pub fn account_config(root: &Path) -> AgentAccountsConfig {
    let config = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    config.disable_claude_keychain_fallback().unwrap();
    // A synthetic provider-native identity so Claude dispatch is admitted.
    std::fs::create_dir_all(&config.claude_config_dir).unwrap();
    std::fs::write(
        &config.claude_config_file,
        json!({"oauthAccount": {"accountUuid": "claude-fixture", "emailAddress": "claude@example.test"}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        config.claude_config_dir.join(".credentials.json"),
        json!({"claudeAiOauth": {"accessToken": "synthetic"}}).to_string(),
    )
    .unwrap();
    config
}

pub fn fixture_path(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures")
        .join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

pub fn run_request(prompt: &str, cwd: &Path) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.display().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![],
        worktree: None,
        resume: None,
    }
}

pub struct Fixture {
    pub root: TempDir,
    pub core: EngineCore,
    pub rpc: Arc<EngineRpc>,
    pub client: RpcClient,
    pub provider: Provider,
    pub project_a: PathBuf,
    pub project_b: PathBuf,
    /// The main chat that drives the scenario (project-a).
    pub main: String,
    /// Another main chat of project-a (a spoofing target).
    pub other: String,
    /// A main chat of project-b.
    pub foreign: String,
}

pub fn boot(provider: Provider, harness: Arc<dyn Harness>) -> Fixture {
    let root = TempDir::new().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(harness);
    // A local profile is the one the board chat model and review E2Es run
    // on: its store carries the write epoch board writes are admitted by.
    let engine_root = root.path().join("engine");
    std::fs::create_dir_all(&engine_root).unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(&engine_root).unwrap(),
        registry,
        provider.harness,
        None,
        account_config(root.path()),
    )
    .unwrap();
    // One RPC service: it is the engine's cross-task/board backend too.
    let rpc = core.rpc_service();
    let client = luna_rpc::memory_client(rpc.clone());
    // CLIs report their physical cwd (`/private/var/...` on macOS).
    let base = root.path().canonicalize().unwrap();
    let project_a = base.join("project-a");
    let project_b = base.join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    for (id, path) in [("project-a", &project_a), ("project-b", &project_b)] {
        core.workspace
            .create_space(id, &core.device_id, path.to_str().unwrap(), None, false)
            .unwrap();
    }
    let config = ChatConfig {
        harness: provider.harness,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    };
    let main = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    let foreign = uuid::Uuid::new_v4().to_string();
    for (id, project, path) in [
        (&main, "project-a", &project_a),
        (&other, "project-a", &project_a),
        (&foreign, "project-b", &project_b),
    ] {
        core.workspace
            .create_chat(id, Some(project), None, Some(config.clone()), None)
            .unwrap();
        core.task_identity
            .register_new_task(
                id,
                Some(project),
                None,
                path.to_str(),
                TaskLineage::default(),
            )
            .unwrap();
    }
    Fixture {
        root,
        core,
        rpc,
        client,
        provider,
        project_a,
        project_b,
        main,
        other,
        foreign,
    }
}

impl Fixture {
    /// Write a plan, dispatch it to `chat`, wait for the turn to settle with
    /// no approval card, and return the fake CLI's results.
    pub async fn run_plan(&self, chat: &str, label: &str, plan: &[Value]) -> Value {
        let results = self
            .project_a
            .join(format!("{}{label}.json", self.provider.results_prefix));
        let _ = std::fs::remove_file(&results);
        std::fs::write(
            self.project_a
                .join(format!("{}{label}.json", self.provider.plan_prefix)),
            serde_json::to_vec_pretty(&plan).unwrap(),
        )
        .unwrap();
        self.wait_idle(chat).await;
        let (_, mut events) = self.core.sessions.subscribe(chat, 0).unwrap();
        self.core
            .sessions
            .dispatch(
                chat,
                self.provider.harness,
                run_request(
                    &format!("{}{label}", self.provider.plan_prompt),
                    &self.project_a,
                ),
                Some(format!("board-tools-{label}")),
            )
            .await
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match events.recv().await.unwrap().event {
                    AgentEvent::InputRequested { .. } => {
                        panic!("{label}: routine board operations must not ask for approval")
                    }
                    AgentEvent::Done { status, .. } => break status,
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label}: provider turn did not settle"));
        assert_eq!(status, DoneStatus::Completed, "{label}");
        for _ in 0..800 {
            if let Ok(text) = std::fs::read_to_string(&results) {
                return serde_json::from_str(&text).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{label}: the fake CLI never wrote {}", results.display());
    }

    /// No live run (the previous provider process retired).
    pub async fn wait_idle(&self, chat: &str) {
        tokio::time::timeout(Duration::from_secs(60), async {
            while self.core.sessions.has_live_run(chat) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{chat}: run never retired"));
    }

    /// A plain first turn (a child or worker chat) finished and recorded the
    /// LUNA instructions it was launched with.
    pub async fn plain_turn_instructions(&self, chat: &str) -> String {
        let file = self
            .project_a
            .join(format!("{}{chat}.txt", self.provider.turn_prefix));
        let text = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&file) {
                    break text;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{chat}: plain turn never ran"));
        self.wait_idle(chat).await;
        text
    }

    pub fn authority(&self, task_id: &str) -> CrossTaskAuthority {
        CrossTaskAuthority {
            caller_task_id: task_id.into(),
            local_environment_id: self.core.device_id.clone(),
            visible_project_ids: HashSet::from(["project-a".to_owned()]),
            allow_projectless: false,
            granted_scopes: HashSet::from([
                "threads.read".to_owned(),
                "board.read".to_owned(),
                "board.write".to_owned(),
            ]),
            explicit_user_request: false,
            delivery: self.provider.delivery,
        }
    }

    pub async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, luna_rpc::RpcError> {
        self.client.call_as::<Value>(method, params).await
    }

    pub async fn rpc_ok(&self, method: &str, params: Value) -> Value {
        match self.rpc_call(method, params.clone()).await {
            Ok(value) => value,
            Err(error) => panic!("{method} {params} failed: {error}"),
        }
    }

    pub fn chat_ids(&self) -> HashSet<String> {
        self.core
            .workspace
            .read_chats()
            .unwrap()
            .into_iter()
            .map(|chat| chat.id)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Results and evidence
// ---------------------------------------------------------------------------

/// One tool reply as the fake CLI saw it.
pub struct Reply<'a> {
    pub raw: &'a Value,
    pub is_error: bool,
    pub text: &'a str,
    pub json: Option<Value>,
}

pub fn reply<'a>(results: &'a Value, index: usize) -> Reply<'a> {
    let raw = &results[index.to_string()];
    assert_eq!(raw["status"], 200, "step {index}: {raw:#}");
    let text = raw["text"].as_str().unwrap_or_default();
    Reply {
        raw,
        is_error: raw["isError"].as_bool().expect("an MCP tool result"),
        text,
        json: serde_json::from_str(text).ok(),
    }
}

impl Reply<'_> {
    pub fn ok(&self) -> Result<&Value, String> {
        match (&self.json, self.is_error) {
            (Some(json), false) => Ok(json),
            _ => Err(format!("expected success, got {}", self.text)),
        }
    }

    pub fn code(&self) -> Option<&str> {
        self.json.as_ref()?.get("code")?.as_str()
    }

    /// A structured board error with one of these codes.
    pub fn board_error(&self, codes: &[&str]) -> Result<&Value, String> {
        match (&self.json, self.is_error, self.code()) {
            (Some(json), true, Some(code)) if codes.contains(&code) => Ok(json),
            _ => Err(format!("expected board error {codes:?}, got {}", self.text)),
        }
    }

    /// Refused by the provider fence: the tool is not attached to this run.
    pub fn refused(&self) -> Result<(), String> {
        if self.is_error && self.code().is_none() && self.text.contains(NOT_ATTACHED) {
            Ok(())
        } else {
            Err(format!(
                "expected a not-attached refusal, got {}",
                self.text
            ))
        }
    }

    /// A slice operation: either its receipt, or `notImplemented` (or another
    /// structured board error) that changed nothing. Never a transport error.
    pub fn receipt_or_board_error(&self) -> Result<Option<&Value>, String> {
        match (&self.json, self.is_error, self.code()) {
            (Some(json), false, _) => Ok(Some(json)),
            (Some(_), true, Some(_)) => Ok(None),
            _ => Err(format!(
                "expected a receipt or a board error, got {}",
                self.text
            )),
        }
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Scenario evidence: steps with expected/observed values and hashes.
pub struct Evidence {
    provider: &'static str,
    steps: Vec<Value>,
    artifacts: serde_json::Map<String, Value>,
}

impl Evidence {
    pub fn new(provider: &'static str) -> Self {
        Self {
            provider,
            steps: Vec::new(),
            artifacts: serde_json::Map::new(),
        }
    }

    /// Record a checked step; panics with the observed value on failure.
    pub fn check(&mut self, step: &str, expected: &str, observed: Result<Value, String>) {
        match observed {
            Ok(observed) => self.steps.push(json!({
                "step": step,
                "expected": expected,
                "observed": observed,
                "passed": true,
            })),
            Err(error) => panic!("[{}] {step}: expected {expected}: {error}", self.provider),
        }
    }

    pub fn artifact(&mut self, name: &str, value: &Value) {
        let bytes = serde_json::to_vec(value).unwrap();
        self.artifacts.insert(
            name.into(),
            json!({"sha256": sha256(&bytes), "bytes": bytes.len()}),
        );
    }

    pub fn text_artifact(&mut self, name: &str, text: &str) {
        self.artifacts.insert(
            name.into(),
            json!({"sha256": sha256(text.as_bytes()), "bytes": text.len()}),
        );
    }

    pub fn write(self) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/tools");
        std::fs::create_dir_all(&dir).unwrap();
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
        let scenario = json!({
            "suite": "board_tools_e2e",
            "provider": self.provider,
            "contract": "docs/design/project-board/pb0-contract.md",
            "commit": commit,
            "recordedAtMs": chrono::Utc::now().timestamp_millis(),
            "steps": self.steps,
            "artifacts": self.artifacts,
        });
        let body = serde_json::to_vec_pretty(&scenario).unwrap();
        std::fs::write(dir.join(format!("{}.json", self.provider)), &body).unwrap();
        let manifest_path = dir.join("manifest.json");
        let mut manifest: Value = std::fs::read(&manifest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({"suite":"board_tools_e2e","providers":{}}));
        manifest["suite"] = json!("board_tools_e2e");
        manifest["providers"][self.provider] = json!({
            "file": format!("{}.json", self.provider),
            "sha256": sha256(&body),
            "steps": scenario["steps"].as_array().unwrap().len(),
            "commit": commit,
            "passed": true,
        });
        std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }
}

fn names(handshake: &Value) -> Vec<String> {
    handshake["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The listed board tools are exactly the audience's (luna_ prefixes that
/// OpenCode strips are restored by the caller).
fn board_names(handshake: &Value) -> Vec<String> {
    let listed = names(handshake);
    let mut board: Vec<String> = luna_proto::BOARD_TOOL_NAMES
        .iter()
        .filter(|name| listed.iter().any(|listed| listed == *name))
        .map(|name| (*name).to_owned())
        .collect();
    board.sort();
    board
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    names.sort();
    names
}

fn schema_enum(handshake: &Value, tool: &str, property: &str) -> Vec<String> {
    handshake["schemas"][tool]["properties"][property]["enum"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn expect(condition: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(what()) }
}

fn has_board_block(instructions: &str, audience: &str, task: &str) -> Result<Value, String> {
    expect(instructions.contains("LUNA board context"), || {
        "the LUNA board context section is missing".into()
    })?;
    expect(
        instructions.contains(&format!("\"audience\":\"{audience}\"")),
        || format!("audience {audience} missing from the board context"),
    )?;
    expect(
        instructions.contains(&format!("\"taskId\":\"{task}\"")),
        || format!("task {task} missing from the board context"),
    )?;
    Ok(json!({"audience": audience, "taskId": task, "chars": instructions.chars().count()}))
}

/// The board tools the engine attaches for an authority, sorted, and their
/// serialized size as an MCP `tools/list` carries them (name, description,
/// input schema) — the per-chat schema cost of the board.
fn board_catalog(
    tools: &CrossTaskTools<Arc<EngineRpc>>,
    authority: &CrossTaskAuthority,
) -> (Vec<String>, usize) {
    let (attached, _) = tools.catalog(authority);
    let board: Vec<_> = attached
        .iter()
        .filter(|tool| luna_proto::BOARD_TOOL_NAMES.contains(&tool.name.as_str()))
        .collect();
    let bytes = board
        .iter()
        .map(|tool| {
            serde_json::to_vec(&json!({
                "name": tool.name, "description": tool.description, "inputSchema": tool.input_schema,
            }))
            .unwrap()
            .len()
        })
        .sum();
    let mut names: Vec<String> = board.iter().map(|tool| tool.name.clone()).collect();
    names.sort();
    (names, bytes)
}

fn catalog_size(audience: &str, names: &[String], bytes: usize) -> Value {
    eprintln!(
        "[board tools] {audience}: {} tools, {bytes} bytes (~{} tokens)",
        names.len(),
        bytes / 4
    );
    json!({"audience": audience, "tools": names.len(), "bytes": bytes, "approxTokens": bytes / 4})
}

fn tool(name: &str, arguments: Value) -> Value {
    json!({"tool": name, "arguments": arguments})
}

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

pub async fn board_tools_scenario(f: &Fixture) {
    let mut evidence = Evidence::new(f.provider.name);
    let main = f.main.clone();
    let other = f.other.clone();

    // A ticket of another project, created by the owner through the RPC.
    let foreign = f
        .rpc_ok(
            "BoardCreate",
            json!({"requestId":"foreign-1","projectId":"project-b","title":"Foreign ticket"}),
        )
        .await;
    let foreign_id = foreign["workItemId"].as_str().unwrap().to_owned();
    let chats_before = f.chat_ids();

    // ---- run 1: the main chat on its initial launch ------------------------
    let title = "Search omits archived projects";
    let brief = "Respect the Include archived toggle.\n\nAcceptance: archived projects appear only when the toggle is on.";
    let create =
        json!({"requestId":"create-1","title":title,"description":brief,"labels":["search"]});
    let run1_plan = vec![
        tool("luna_status", json!({})),                  // 0
        tool("get_workspace", json!({"topic":"board"})), // 1
        tool("board_create", create.clone()),            // 2
        tool("board_create", create.clone()),            // 3
        tool(
            "board_create",
            json!({"requestId":"create-1","title":"A different ticket"}),
        ), // 4
        tool("board_read", json!({"workItemId":"$ref:2.key"})), // 5
        tool(
            "board_update",
            json!({"requestId":"update-1","workItemId":"$ref:2.workItemId",
            "expectedRevision":1,"changes":{"description":"Respect the Include archived toggle; keep current behavior when it is off."}}),
        ), // 6
        tool(
            "board_update",
            json!({"requestId":"update-stale","workItemId":"$ref:2.workItemId",
            "expectedRevision":1,"changes":{"description":"A conflicting brief"}}),
        ), // 7
        tool("board_list", json!({})),                   // 8
        tool("board_execution_options", json!({})),      // 9
        tool(
            "board_move",
            json!({"requestId":"move-ready","workItemId":"$ref:2.workItemId","to":"ready","destination":"sideChat"}),
        ), // 10
        tool(
            "board_control",
            json!({"requestId":"stop-1","workItemId":"$ref:2.workItemId","action":"stop"}),
        ), // 11
        tool(
            "board_control",
            json!({"requestId":"accept-1","workItemId":"$ref:2.workItemId","action":"accept"}),
        ), // 12
        tool(
            "board_move",
            json!({"requestId":"to-done","workItemId":"$ref:2.workItemId","to":"done"}),
        ), // 13
        tool(
            "board_create",
            json!({"requestId":"spoof-origin","title":"Spoofed","origin":{"kind":"chat","taskId":other}}),
        ), // 14
        tool(
            "create_side_chat",
            json!({"requestId":"spoof-side","callerTaskId":other,"prompt":"x"}),
        ), // 15
        tool(
            "board_start",
            json!({"requestId":"spoof-dest","workItemId":"$ref:2.workItemId",
            "destination":{"kind":"sideOf","mainTaskId":other}}),
        ), // 16
        tool(
            "board_convert_chat",
            json!({"requestId":"spoof-convert","taskId":other,"description":"x"}),
        ), // 17
        tool("board_read", json!({"workItemId":foreign_id})), // 18
        tool("board_list", json!({"projectId":"project-b"})), // 19
        tool(
            "board_create",
            json!({"requestId":"cross-create","projectId":"project-b","title":"Elsewhere"}),
        ), // 20
        tool(
            "board_update",
            json!({"requestId":"cross-update","workItemId":foreign_id,
            "expectedRevision":1,"changes":{"title":"Hijacked"}}),
        ), // 21
        tool(
            "review_submit",
            json!({"requestId":"not-a-reviewer","verdict":"passed","basisPacketHash":"x","summary":"looks good"}),
        ), // 22
        tool(
            "board_draft_checkpoint",
            json!({"expectedVersion":0,"proposal":{}}),
        ), // 23
        tool(
            "create_side_chat",
            json!({"requestId":"side-ctx","context":"withContext","title":"Context side","start":false}),
        ), // 24
        tool(
            "create_thread",
            json!({"prompt":"Side helper: wait for instructions.","idempotencyKey":"side-helper-1","title":"Side helper"}),
        ), // 25
    ];
    let run1 = f.run_plan(&main, "board1", &run1_plan).await;
    evidence.artifact("run1-results", &run1);

    evidence.check(
        "initial launch lists exactly the main-chat board tools",
        "every main-chat board tool; no review_submit or board_draft_checkpoint",
        {
            let board = board_names(&run1["handshake"]);
            expect(board == sorted(MAIN_TOOLS), || format!("{board:?}")).map(|_| json!(board))
        },
    );
    evidence.check(
        "board tools are annotated read-only exactly when they are",
        "readOnlyHint true only for list/read/options/review_status/issue_search",
        {
            let hints = &run1["handshake"]["readOnly"];
            let wrong: Vec<&str> = MAIN_TOOLS
                .iter()
                .copied()
                .filter(|name| hints[*name] != json!(READ_ONLY_TOOLS.contains(name)))
                .collect();
            expect(wrong.is_empty(), || format!("{wrong:?} in {hints}"))
                .map(|_| json!(READ_ONLY_TOOLS))
        },
    );
    evidence.check(
        "main-chat schemas offer withContext and a side-chat destination",
        "create_side_chat.context = [fresh, withContext]; board_start.destination has sideChat",
        {
            let context = schema_enum(&run1["handshake"], "create_side_chat", "context");
            let destination = schema_enum(&run1["handshake"], "board_start", "destination");
            expect(
                context == ["fresh", "withContext"]
                    && destination.contains(&"sideChat".into())
                    && !destination.contains(&"sibling".into()),
                || format!("context {context:?} destination {destination:?}"),
            )
            .map(|_| json!({"context": context, "destination": destination}))
        },
    );
    let instructions1 = run1[f.provider.instructions_key]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    evidence.text_artifact("run1-instructions", &instructions1);
    evidence.check(
        "initial launch carries the board context block",
        "LUNA board context with audience mainChat for this chat",
        has_board_block(&instructions1, "mainChat", &main),
    );
    evidence.check(
        "luna_status grants board scopes",
        "board.read and board.write",
        {
            let status = reply(&run1, 0);
            status.ok().and_then(|status| {
                let scopes = &status["grantedScopes"];
                expect(
                    ["threads.read", "board.read", "board.write"]
                        .iter()
                        .all(|scope| scopes.as_array().unwrap().contains(&json!(scope))),
                    || format!("{scopes}"),
                )?;
                expect(status["threadId"] == json!(main), || format!("{status}"))?;
                Ok(scopes.clone())
            })
        },
    );
    evidence.check(
        "get_workspace board guide returns the canonical main context",
        "audience mainChat, role main, display container = itself, withContext permitted",
        reply(&run1, 1).ok().and_then(|workspace| {
            let board = &workspace["board"];
            expect(
                board["audience"] == "mainChat"
                    && board["role"] == "main"
                    && board["taskId"] == json!(main)
                    && board["displayContainerId"] == json!(main)
                    && board["projectId"] == "project-a"
                    && board["permitted"]["createSideWithContext"] == true
                    && board["permitted"]["reparent"] != true
                    && workspace["guide"]["topic"] == "board",
                || format!("{workspace:#}"),
            )?;
            Ok(board.clone())
        }),
    );
    let created = reply(&run1, 2).ok().unwrap().clone();
    let ticket = created["workItemId"].as_str().unwrap().to_owned();
    let key = created["key"].as_str().unwrap().to_owned();
    evidence.check(
        "board_create saves a Backlog ticket attributed to this chat",
        "wi- id, key, backlog, revision 1, agent actor and chat origin = this chat, no chat",
        expect(
            ticket.starts_with("wi-")
                && luna_proto::parse_board_key(&key).is_some()
                && created["column"] == "backlog"
                && created["revision"] == 1
                && created["actor"]["kind"] == "agent"
                && created["actor"]["taskId"] == json!(main)
                && created["item"]["origin"] == json!({"kind":"chat","taskId":main})
                && created["navigation"]["kind"] == "noChat",
            || format!("{created:#}"),
        )
        .map(|_| json!({"workItemId": ticket, "key": key})),
    );
    evidence.check(
        "a repeated create with the same requestId returns the same receipt",
        "identical receipt (one ticket, one event)",
        reply(&run1, 3).ok().and_then(|again| {
            expect(again == &created, || format!("{again:#}")).map(|_| json!(again["eventSeq"]))
        }),
    );
    evidence.check(
        "the same requestId with a different payload is refused",
        "idempotencyConflict",
        reply(&run1, 4)
            .board_error(&["idempotencyConflict"])
            .map(|error| error["code"].clone()),
    );
    evidence.check(
        "board_read by key returns the ticket with its source link",
        "same ticket; Source link to this chat; content marked untrusted",
        reply(&run1, 5).ok().and_then(|detail| {
            expect(
                detail["view"]["item"]["id"] == json!(ticket)
                    && detail["contentUntrusted"] == true
                    && detail["links"].as_array().is_some_and(|links| {
                        links
                            .iter()
                            .any(|link| link["role"] == "source" && link["taskId"] == json!(main))
                    }),
                || format!("{detail:#}"),
            )
            .map(|_| json!(detail["view"]["item"]["revision"]))
        }),
    );
    evidence.check(
        "board_update applies a field change against the expected revision",
        "revision 2, brief revision 2",
        reply(&run1, 6).ok().and_then(|receipt| {
            expect(
                receipt["revision"] == 2 && receipt["item"]["briefRevision"] == 2,
                || format!("{receipt:#}"),
            )
            .map(|_| json!(receipt["revision"]))
        }),
    );
    evidence.check(
        "a stale edit of the same field returns the current ticket",
        "staleRevision carrying revision 2",
        reply(&run1, 7)
            .board_error(&["staleRevision"])
            .and_then(|error| {
                expect(error["current"]["item"]["revision"] == 2, || {
                    format!("{error:#}")
                })
                .map(|_| json!(error["code"]))
            }),
    );
    evidence.check(
        "board_list is bounded, scoped and untrusted",
        "the ticket in project-a, contentUntrusted",
        reply(&run1, 8).ok().and_then(|page| {
            let items = page["items"].as_array().cloned().unwrap_or_default();
            expect(
                page["contentUntrusted"] == true
                    && items.iter().any(|item| item["key"] == json!(key))
                    && items.iter().all(|item| item["projectId"] == "project-a")
                    && items.len() <= 25,
                || format!("{page:#}"),
            )
            .map(|_| json!(items.len()))
        }),
    );
    evidence.check(
        "board_execution_options answers or says why not",
        "harness options, or a structured board error (nothing invented)",
        {
            let options = reply(&run1, 9);
            options
                .receipt_or_board_error()
                .map(|receipt| match receipt {
                    Some(options) => {
                        json!({"harnesses": options["harnesses"].as_array().map(Vec::len)})
                    }
                    None => json!({"code": options.code()}),
                })
        },
    );
    let moved = reply(&run1, 10);
    let move_receipt = moved.receipt_or_board_error();
    evidence.check(
        "board_move to Ready goes through the lifecycle with the agent's authority",
        "a receipt attributed to this chat, or a structured board error with no effect",
        move_receipt.clone().and_then(|receipt| match receipt {
            Some(receipt) => expect(
                receipt["mutation"]["workItemId"] == json!(ticket)
                    && receipt["mutation"]["actor"]["taskId"] == json!(main)
                    && receipt["effect"]["kind"].is_string(),
                || format!("{receipt:#}"),
            )
            .map(|_| json!({"implemented": true, "effect": receipt["effect"]["kind"]})),
            None => Ok(json!({"implemented": false, "code": moved.code(), "reply": moved.text})),
        }),
    );
    let started = matches!(move_receipt, Ok(Some(_)));
    evidence.check(
        "board_control stop goes through the shared Stop with the agent's authority",
        "a control receipt for this ticket, or a structured board error",
        {
            let stop = reply(&run1, 11);
            stop.receipt_or_board_error()
                .and_then(|receipt| match receipt {
                    Some(receipt) => expect(
                        receipt["mutation"]["workItemId"] == json!(ticket)
                            && receipt["action"] == "stop",
                        || format!("{receipt:#}"),
                    )
                    .map(|_| json!({"implemented": true})),
                    None => Ok(json!({"implemented": false, "code": stop.code()})),
                })
        },
    );
    evidence.check(
        "acceptance stays the user's decision",
        "invalidRole",
        reply(&run1, 12)
            .board_error(&["invalidRole"])
            .map(|error| error["operation"].clone()),
    );
    evidence.check(
        "Done is engine-owned",
        "invalidInput on to",
        reply(&run1, 13)
            .board_error(&["invalidInput"])
            .and_then(|error| {
                expect(error["field"] == "to", || format!("{error}"))
                    .map(|_| error["field"].clone())
            }),
    );
    for (index, step, expected) in [
        (
            14,
            "a model-supplied foreign origin is refused",
            &["notAuthorized"][..],
        ),
        (
            15,
            "a model-supplied foreign caller id is refused",
            &["notAuthorized"][..],
        ),
        (
            16,
            "a destination naming another chat as parent is refused",
            &["invalidDestination"][..],
        ),
        (
            17,
            "converting another chat is refused",
            &["notAuthorized"][..],
        ),
        (
            18,
            "reading another project's ticket is refused",
            &["notAuthorized"][..],
        ),
        (
            19,
            "listing another project's board is refused",
            &["notAuthorized"][..],
        ),
        (
            20,
            "creating in another project is refused",
            &["notAuthorized"][..],
        ),
        (
            21,
            "editing another project's ticket is refused",
            &["notAuthorized"][..],
        ),
    ] {
        evidence.check(
            step,
            expected[0],
            reply(&run1, index)
                .board_error(expected)
                .map(|error| error["code"].clone()),
        );
    }
    evidence.check(
        "review_submit is hidden from and refused for a main chat",
        "not attached",
        reply(&run1, 22).refused().map(|_| json!("refused")),
    );
    evidence.check(
        "board_draft_checkpoint is hidden from and refused for a main chat",
        "not attached",
        reply(&run1, 23).refused().map(|_| json!("refused")),
    );
    let side_ctx = reply(&run1, 24);
    let side_ctx_created = side_ctx.receipt_or_board_error().ok().flatten().cloned();
    evidence.check(
        "a main chat may create a side chat with context",
        "a side chat under this chat from its own mid-turn tool call, with a reported context origin",
        side_ctx
            .receipt_or_board_error()
            .and_then(|receipt| match receipt {
                // The caller is its own active turn: the engine snapshots the
                // visible transcript so far (or forks natively when the
                // provider can) instead of waiting for the turn to settle.
                Some(created) => expect(
                    created["role"] == "side"
                        && created["mainParentId"] == json!(main)
                        && matches!(
                            created["contextOrigin"].as_str(),
                            Some("contextSnapshot" | "nativeFork")
                        ),
                    || format!("{created:#}"),
                )
                .map(|_| json!({"implemented": true, "contextOrigin": created["contextOrigin"]})),
                None => Err(format!("with-context creation was refused: {}", side_ctx.text)),
            }),
    );
    let child = reply(&run1, 25).ok().unwrap()["taskId"]
        .as_str()
        .unwrap()
        .to_owned();

    // Store truth through the owner's RPC, independent of tool replies.
    let tickets = f
        .rpc_ok(
            "BoardList",
            json!({"scope":{"kind":"project","projectId":"project-a"}}),
        )
        .await;
    evidence.check(
        "two creates with one key stored one ticket",
        "exactly one ticket titled like the request in project-a",
        {
            let items = tickets["items"].as_array().cloned().unwrap_or_default();
            let same: Vec<_> = items
                .iter()
                .filter(|item| item["item"]["title"] == title)
                .collect();
            expect(same.len() == 1 && items.len() == 1, || {
                format!("{tickets:#}")
            })
            .map(|_| json!(items.len()))
        },
    );
    let read = f.rpc_ok("BoardRead", json!({"workItemId": ticket})).await;
    if !started {
        evidence.check(
            "an unimplemented move changed nothing",
            "still Backlog, no working link",
            expect(
                read["view"]["item"]["column"] == "backlog"
                    && !read["links"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|link| link["role"] == "working"),
                || format!("{read:#}"),
            )
            .map(|_| json!(read["view"]["item"]["column"])),
        );
    }
    let foreign_after = f
        .rpc_ok("BoardRead", json!({"workItemId": foreign_id}))
        .await;
    evidence.check(
        "the other project's ticket is untouched",
        "revision 1, original title",
        expect(
            foreign_after["view"]["item"]["revision"] == 1
                && foreign_after["view"]["item"]["title"] == "Foreign ticket",
            || format!("{foreign_after:#}"),
        )
        .map(|_| json!(1)),
    );

    // ---- the side chat's first turn: its launch context ------------------
    let child_instructions = f.plain_turn_instructions(&child).await;
    evidence.text_artifact("side-first-turn-instructions", &child_instructions);
    evidence.check(
        "a side chat's first turn carries its side-chat board context",
        "audience sideChat with the main parent",
        has_board_block(&child_instructions, "sideChat", &child).and_then(|observed| {
            expect(
                child_instructions.contains(&format!("\"mainParentId\":\"{main}\"")),
                || "main parent missing".into(),
            )
            .map(|_| observed)
        }),
    );

    // ---- run 2: the main chat resumed, then compacted ----------------------
    let run2_plan = vec![
        json!({"relist": true}),                           // 0
        tool("get_workspace", json!({"topic":"board"})),   // 1
        tool("board_read", json!({"workItemId": ticket})), // 2
        (f.provider.compaction)(),                         // 3
        json!({"relist": true}),                           // 4
        tool("get_workspace", json!({"topic":"board"})),   // 5
        tool("board_create", create.clone()),              // 6
    ];
    f.wait_idle(&main).await;
    let run2 = f.run_plan(&main, "board2", &run2_plan).await;
    evidence.artifact("run2-results", &run2);
    let instructions2 = run2[f.provider.instructions_key]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    evidence.text_artifact("run2-instructions", &instructions2);
    evidence.check(
        "a resumed run rediscovers the board tools on its new endpoint",
        "the same main-chat board tools",
        {
            let initial = board_names(&run2["handshake"]);
            let relisted = board_names(&run2["0"]);
            expect(initial == sorted(MAIN_TOOLS) && relisted == initial, || {
                format!("{initial:?} / {relisted:?}")
            })
            .map(|_| json!(initial.len()))
        },
    );
    evidence.check(
        "the resumed run carries the board context block",
        "LUNA board context with audience mainChat",
        has_board_block(&instructions2, "mainChat", &main),
    );
    evidence.check(
        "the resumed run reaches the same ticket",
        "board_read ok",
        reply(&run2, 2)
            .ok()
            .and_then(|detail| {
                expect(detail["view"]["item"]["id"] == json!(ticket), || {
                    format!("{detail}")
                })
            })
            .map(|_| json!(ticket)),
    );
    evidence.check(
        "after compaction the catalog and context are rediscovered",
        "same board tools; get_workspace board context for this chat",
        {
            let relisted = board_names(&run2["4"]);
            reply(&run2, 5).ok().and_then(|workspace| {
                expect(
                    relisted == sorted(MAIN_TOOLS)
                        && workspace["board"]["taskId"] == json!(main)
                        && workspace["board"]["audience"] == "mainChat",
                    || format!("{relisted:?} {workspace:#}"),
                )
                .map(|_| workspace["board"]["audience"].clone())
            })
        },
    );
    evidence.check(
        "a retried create after resume still returns the original receipt",
        "identical to the first receipt",
        reply(&run2, 6).ok().and_then(|again| {
            expect(again == &created, || format!("{again:#}")).map(|_| json!(again["workItemId"]))
        }),
    );

    // ---- run S: the side chat as the caller -------------------------------
    let side_plan = vec![
        json!({"relist": true}),                         // 0
        tool("get_workspace", json!({"topic":"board"})), // 1
        tool(
            "create_side_chat",
            json!({"requestId":"nested-ctx","context":"withContext","prompt":"x"}),
        ), // 2
        tool(
            "create_side_chat",
            json!({"requestId":"nested-ctx-2","context":{"kind":"withContext"}}),
        ), // 3
        tool(
            "board_start",
            json!({"requestId":"side-dest","workItemId":ticket,"destination":"sideChat"}),
        ), // 4
        tool(
            "board_start",
            json!({"requestId":"side-dest-2","workItemId":ticket,
            "destination":{"kind":"sideOf","mainTaskId":child}}),
        ), // 5
        tool(
            "create_main_chat",
            json!({"requestId":"handoff-missing","title":"Next"}),
        ), // 6
        tool(
            "create_side_chat",
            json!({"requestId":"sibling-1","title":"Sibling","start":false}),
        ), // 7
        tool(
            "create_main_chat",
            json!({"requestId":"handoff-1","handoffBrief":"Goal: finish the archived-project search fix. Done: toggle wired. Next: tests.","title":"Handoff","start":false}),
        ), // 8
        tool(
            "board_create",
            json!({"requestId":"side-create","title":"Follow-up from the side chat"}),
        ), // 9
        tool("board_create", create.clone()),            // 10
        tool(
            "review_submit",
            json!({"requestId":"side-review","verdict":"passed","basisPacketHash":"x","summary":"ok"}),
        ), // 11
    ];
    let side = f.run_plan(&child, "board-side", &side_plan).await;
    evidence.artifact("side-results", &side);
    let side_instructions = side[f.provider.instructions_key]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    evidence.text_artifact("side-run-instructions", &side_instructions);
    evidence.check(
        "a side chat's run carries its board context",
        "audience sideChat",
        has_board_block(&side_instructions, "sideChat", &child),
    );
    evidence.check(
        "a side chat's schemas offer no context forks, no side-of-self and no create_thread",
        "no create_side_chat.context; destination has sibling, not sideChat; handoffBrief required; create_thread not listed",
        {
            let listed = &side["0"];
            let schemas = &listed["schemas"];
            let destination = schema_enum(listed, "board_start", "destination");
            let required = schemas["create_main_chat"]["required"].clone();
            expect(
                board_names(listed) == sorted(MAIN_TOOLS)
                    && !names(listed).iter().any(|name| name == "create_thread")
                    && schemas["create_side_chat"]["properties"]
                        .get("context")
                        .is_none()
                    && schemas["create_side_chat"]["properties"]
                        .get("checkpointMessageId")
                        .is_none()
                    && destination.contains(&"sibling".into())
                    && !destination.contains(&"sideChat".into())
                    && required
                        .as_array()
                        .is_some_and(|required| required.contains(&json!("handoffBrief"))),
                || format!("{destination:?} {:#}", schemas["create_side_chat"]),
            )
            .map(|_| json!({"destination": destination}))
        },
    );
    evidence.check(
        "get_workspace returns the canonical side context",
        "audience sideChat, role side, main parent and display container = the main chat",
        reply(&side, 1).ok().and_then(|workspace| {
            let board = &workspace["board"];
            expect(
                board["audience"] == "sideChat"
                    && board["role"] == "side"
                    && board["taskId"] == json!(child)
                    && board["mainParentId"] == json!(main)
                    && board["displayContainerId"] == json!(main)
                    && board["permitted"]["createSideWithContext"] != true
                    && board["permitted"]["createSiblingFresh"] == true,
                || format!("{workspace:#}"),
            )
            .map(|_| board.clone())
        }),
    );
    for (index, step) in [
        (2, "a side chat cannot request withContext"),
        (3, "a side chat cannot request withContext in object form"),
    ] {
        evidence.check(
            step,
            "invalidRole allowing only fresh",
            reply(&side, index)
                .board_error(&["invalidRole"])
                .and_then(|error| {
                    expect(error["allowed"] == json!(["fresh"]), || format!("{error}"))
                        .map(|_| error["operation"].clone())
                }),
        );
    }
    for (index, step) in [
        (4, "a side chat cannot place work in its own side chat"),
        (5, "a side chat cannot name itself as a main parent"),
    ] {
        evidence.check(
            step,
            "invalidDestination",
            reply(&side, index)
                .board_error(&["invalidDestination"])
                .map(|error| error["code"].clone()),
        );
    }
    evidence.check(
        "a side chat's handoff needs a written brief",
        "invalidInput on handoffBrief",
        reply(&side, 6)
            .board_error(&["invalidInput"])
            .and_then(|error| {
                expect(error["field"] == "handoffBrief", || format!("{error}"))
                    .map(|_| error["field"].clone())
            }),
    );
    let sibling = reply(&side, 7);
    let sibling_created = sibling.receipt_or_board_error().ok().flatten().cloned();
    evidence.check(
        "a side chat's create_side_chat makes a sibling, never a nested chat",
        "a side chat under the main parent, or notImplemented",
        sibling
            .receipt_or_board_error()
            .and_then(|receipt| match receipt {
                Some(created) => expect(
                    created["role"] == "side"
                        && created["mainParentId"] == json!(main)
                        && created["taskId"] != json!(child),
                    || format!("{created:#}"),
                )
                .map(|_| json!({"implemented": true})),
                None => expect(sibling.code() == Some("notImplemented"), || {
                    sibling.text.into()
                })
                .map(|_| json!({"implemented": false})),
            }),
    );
    let handoff = reply(&side, 8);
    let handoff_created = handoff.receipt_or_board_error().ok().flatten().cloned();
    evidence.check(
        "a side chat's handoff creates a distinct main chat and stays in place",
        "a new main chat, or notImplemented",
        handoff
            .receipt_or_board_error()
            .and_then(|receipt| match receipt {
                Some(created) => expect(
                    created["role"] == "main"
                        && created["taskId"] != json!(child)
                        && created.get("mainParentId").is_none(),
                    || format!("{created:#}"),
                )
                .map(|_| json!({"implemented": true})),
                None => expect(handoff.code() == Some("notImplemented"), || {
                    handoff.text.into()
                })
                .map(|_| json!({"implemented": false})),
            }),
    );
    evidence.check(
        "a side chat's ticket records the side chat as its source",
        "agent actor and origin = the side chat",
        reply(&side, 9).ok().and_then(|receipt| {
            expect(
                receipt["actor"]["taskId"] == json!(child)
                    && receipt["item"]["origin"] == json!({"kind":"chat","taskId":child}),
                || format!("{receipt:#}"),
            )
            .map(|_| receipt["key"].clone())
        }),
    );
    evidence.check(
        "request keys are scoped per caller",
        "the main chat's key from a side chat creates a different ticket",
        reply(&side, 10).ok().and_then(|receipt| {
            expect(receipt["workItemId"] != json!(ticket), || {
                format!("{receipt}")
            })
            .map(|_| receipt["workItemId"].clone())
        }),
    );
    evidence.check(
        "review_submit is hidden from and refused for a side chat",
        "not attached",
        reply(&side, 11).refused().map(|_| json!("refused")),
    );
    // The side chat stayed where it is: still a side chat of the main chat.
    let child_context = CrossTaskBackend::board_context(&*f.rpc, &child).expect("side context");
    evidence.check(
        "the side caller kept its identity and parent",
        "sideChat under the main chat",
        expect(
            child_context.main_parent_id.as_deref() == Some(main.as_str())
                && child_context.audience == luna_proto::BoardToolAudience::SideChat,
            || format!("{child_context:?}"),
        )
        .map(|_| json!(child_context.main_parent_id)),
    );

    // ---- engine checks regardless of attached schemas ------------------
    let tools = CrossTaskTools::new(f.rpc.clone());
    let cancel = CancellationToken::new();
    let direct = tools
        .invoke(
            &f.authority(&main),
            "review_submit",
            json!({"requestId":"direct-review","verdict":"passed","basisPacketHash":"x","summary":"ok"}),
            &cancel,
        )
        .await;
    evidence.check(
        "a direct engine call of a reviewer-only tool is refused by role",
        "invalidRole",
        match direct {
            Err(luna_engine::cross_task_tools::CrossTaskError::Backend(text))
                if text.contains("\"code\":\"invalidRole\"") =>
            {
                Ok(json!("invalidRole"))
            }
            other => Err(format!("{other:?}")),
        },
    );
    let direct = tools
        .invoke(
            &f.authority(&child),
            "create_side_chat",
            json!({"requestId":"direct-ctx","context":"withContext"}),
            &cancel,
        )
        .await;
    evidence.check(
        "a direct engine call of withContext from a side chat is refused",
        "invalidRole",
        match direct {
            Err(luna_engine::cross_task_tools::CrossTaskError::Backend(text))
                if text.contains("\"code\":\"invalidRole\"") =>
            {
                Ok(json!("invalidRole"))
            }
            other => Err(format!("{other:?}")),
        },
    );
    let (board, main_bytes) = board_catalog(&tools, &f.authority(&main));
    evidence.check(
        "the engine catalog for the main chat matches what the provider listed, within budget",
        "the main-chat board tools in at most 16 KiB (about 4k tokens)",
        expect(
            board == sorted(MAIN_TOOLS) && main_bytes <= CATALOG_BUDGET_BYTES,
            || format!("{board:?} {main_bytes} bytes"),
        )
        .map(|_| catalog_size("mainChat", &board, main_bytes)),
    );
    let (board, side_bytes) = board_catalog(&tools, &f.authority(&child));
    evidence.check(
        "the side chat's catalog is its own, within budget, without create_thread",
        "the side-chat board tools in at most 16 KiB; no legacy create_thread",
        {
            let (attached, _) = tools.catalog(&f.authority(&child));
            expect(
                board == sorted(MAIN_TOOLS)
                    && side_bytes <= CATALOG_BUDGET_BYTES
                    && !attached.iter().any(|tool| tool.name == "create_thread"),
                || format!("{board:?} {side_bytes} bytes"),
            )
            .map(|_| catalog_size("sideChat", &board, side_bytes))
        },
    );
    let (main_attached, _) = tools.catalog(&f.authority(&main));
    evidence.check(
        "a main chat keeps create_thread",
        "create_thread attached for the main chat",
        expect(
            main_attached
                .iter()
                .any(|tool| tool.name == "create_thread"),
            || "create_thread missing".into(),
        )
        .map(|_| json!(true)),
    );
    let diagnostics = f.rpc_ok("BoardToolCatalog", json!({"taskId": main})).await;
    evidence.check(
        "BoardToolCatalog reports the attached board tools and their delivery",
        "available; acknowledged session verified with board tools",
        expect(
            diagnostics["available"] == true
                && diagnostics["acknowledged"]["verified"] == true
                && diagnostics["acknowledged"]["boardTools"]
                    .as_array()
                    .is_some_and(|tools| tools.len() == MAIN_TOOLS.len()),
            || format!("{diagnostics:#}"),
        )
        .map(|_| diagnostics["acknowledged"]["delivery"].clone()),
    );

    // ---- worker, reviewer and authoring audiences (their slices' flows) ----
    // The other main chat converts itself into a ticket's working chat with
    // its own board_convert_chat tool (A3): its catalog becomes the worker's,
    // its context parent-locked.
    let mut review_ticket = ticket.clone();
    match tools
        .invoke(
            &f.authority(&other),
            "board_convert_chat",
            json!({"requestId":"convert-other","title":"Cover the archived toggle",
                "description":"Keep the archived-project toggle covered by tests."}),
            &cancel,
        )
        .await
    {
        Ok(converted) => {
            review_ticket = converted["mutation"]["workItemId"]
                .as_str()
                .unwrap_or(&ticket)
                .to_owned();
            let context = CrossTaskBackend::board_context(&*f.rpc, &other).expect("worker context");
            let (board, bytes) = board_catalog(&tools, &f.authority(&other));
            evidence.check(
                "a ticket's working chat gets the worker tools and a parent-locked context",
                "ticketWorker: status and review tools, no conversion; parent locked; its ticket",
                expect(
                    context.audience == luna_proto::BoardToolAudience::TicketWorker
                        && context.parent_locked
                        && context
                            .ticket
                            .as_ref()
                            .is_some_and(|ticket| ticket.work_item_id == review_ticket)
                        && board == sorted(WORKER_TOOLS),
                    || format!("{context:?} {board:?}"),
                )
                .map(|_| catalog_size("ticketWorker", &board, bytes)),
            );
            let direct = tools
                .invoke(
                    &f.authority(&main),
                    "board_set_status",
                    json!({"requestId":"main-status","workItemId": review_ticket,"kind":"progress","summary":"x"}),
                    &cancel,
                )
                .await;
            evidence.check(
                "status reports are a working chat's tool, not an ordinary chat's",
                "invalidRole for the main chat",
                match direct {
                    Err(luna_engine::cross_task_tools::CrossTaskError::Backend(text))
                        if text.contains("\"code\":\"invalidRole\"") =>
                    {
                        Ok(json!("invalidRole"))
                    }
                    other => Err(format!("{other:?}")),
                },
            );
        }
        Err(error) => evidence.check(
            "worker audience (chat-model slice conversion)",
            "a conversion receipt or a structured board error",
            match error {
                luna_engine::cross_task_tools::CrossTaskError::Backend(text) => {
                    serde_json::from_str::<Value>(&text)
                        .ok()
                        .filter(|error| error["code"].is_string())
                        .map(|error| json!({"skipped": error["code"], "reason": error["reason"]}))
                        .ok_or(text)
                }
                other => Err(format!("{other:?}")),
            },
        ),
    }
    // Review needs a live started execution (a generation that was not
    // stopped). With the lifecycle slice present, the main chat creates and
    // starts a fresh ticket with its own tools, and that ticket's worker
    // submits it with review_start (checks and summary). Otherwise the
    // converted ticket shows the typed refusal through the owner's RPC.
    let mut review_worker: Option<String> = None;
    let mut reviewer_task: Option<String> = None;
    let review_started = if started {
        let created = tools
            .invoke(
                &f.authority(&main),
                "board_create",
                json!({"requestId":"review-target","title":"Review target",
                    "description":"Exercise the reviewer audience."}),
                &cancel,
            )
            .await
            .expect("board_create for the review target");
        let id = created["workItemId"].as_str().unwrap().to_owned();
        let start = tools
            .invoke(
                &f.authority(&main),
                "board_start",
                json!({"requestId":"review-target-start","workItemId": id,"destination":"sideChat"}),
                &cancel,
            )
            .await;
        evidence.check(
            "the main chat starts a ticket for the reviewer audience",
            "a start receipt",
            start
                .map(|receipt| json!(receipt["mutation"]["column"]))
                .map_err(|error| format!("{error:?}")),
        );
        let read = f.rpc_ok("BoardRead", json!({"workItemId": id})).await;
        review_worker = read["links"].as_array().and_then(|links| {
            links
                .iter()
                .find(|link| link["role"] == "working")
                .and_then(|link| link["taskId"].as_str().map(str::to_owned))
        });
        let worker = review_worker
            .clone()
            .expect("the started ticket's working chat");
        tools
            .invoke(
                &f.authority(&worker),
                "review_start",
                json!({"requestId":"review-1",
                    "checks":[{"name":"unit","command":"cargo test","passed":true,"exitCode":0}],
                    "summary":"Toggle respected; checks pass."}),
                &cancel,
            )
            .await
            .map(|receipt| (id, receipt))
            .map_err(|error| match error {
                luna_engine::cross_task_tools::CrossTaskError::Backend(text) => text,
                other => format!("{other:?}"),
            })
    } else {
        f.rpc_call(
            "BoardReviewStart",
            json!({"requestId":"review-1","workItemId": review_ticket}),
        )
        .await
        .map(|receipt| (review_ticket.clone(), receipt))
        .map_err(|error| match error {
            luna_rpc::RpcError::Failed(message) => message,
            other => other.to_string(),
        })
    };
    match review_started {
        Ok((review_ticket, receipt)) => {
            evidence.check(
                "review_start carries the worker's checks and summary",
                "a review run for the ticket",
                expect(receipt["run"]["workItemId"] == json!(review_ticket), || {
                    format!("{receipt:#}")
                })
                .map(|_| json!(receipt["run"]["state"])),
            );
            // The reviewer chat may be created after the receipt.
            let mut reviewer = receipt["run"]["reviewerTaskId"].as_str().map(str::to_owned);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            while reviewer.is_none() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let status = f
                    .rpc_call("BoardReviewStatus", json!({"workItemId": review_ticket}))
                    .await
                    .unwrap_or_default();
                reviewer = status["runs"].as_array().and_then(|runs| {
                    runs.iter()
                        .find_map(|run| run["reviewerTaskId"].as_str().map(str::to_owned))
                });
            }
            if let Some(reviewer) = reviewer.as_deref() {
                reviewer_task = Some(reviewer.to_owned());
                let context =
                    CrossTaskBackend::board_context(&*f.rpc, reviewer).expect("reviewer context");
                let (board, bytes) = board_catalog(&tools, &f.authority(reviewer));
                evidence.check(
                    "a reviewer side chat gets only reviewer tools",
                    "board_read, review_status, review_submit",
                    expect(
                        context.audience == luna_proto::BoardToolAudience::Reviewer
                            && board == sorted(REVIEWER_TOOLS),
                        || format!("{context:?} {board:?}"),
                    )
                    .map(|_| catalog_size("reviewer", &board, bytes)),
                );
                // Authorized by its recorded run (the run id defaults from
                // the reviewer's context); A5 then judges the submission.
                let submitted = tools
                    .invoke(
                        &f.authority(reviewer),
                        "review_submit",
                        json!({"requestId":"submit-1","verdict":"passed",
                            "basisPacketHash":"not-the-packet","summary":"Looks right."}),
                        &cancel,
                    )
                    .await;
                evidence.check(
                    "review_submit reaches the review slice for the recorded reviewer",
                    "a receipt or a review-slice refusal, never notAuthorized/invalidRole",
                    match submitted {
                        Ok(receipt) => Ok(json!({"accepted": receipt["run"]["state"]})),
                        Err(luna_engine::cross_task_tools::CrossTaskError::Backend(text)) => {
                            serde_json::from_str::<Value>(&text)
                                .ok()
                                .filter(|error| {
                                    error["code"].is_string()
                                        && error["code"] != "notAuthorized"
                                        && error["code"] != "invalidRole"
                                })
                                .map(|error| json!({"refused": error["code"]}))
                                .ok_or(text)
                        }
                        Err(other) => Err(format!("{other:?}")),
                    },
                );
            } else {
                evidence.check(
                    "review start recorded without a reviewer chat yet",
                    "a review run",
                    Ok(json!(receipt["run"]["state"])),
                );
            }
        }
        Err(message) => evidence.check(
            "reviewer audience (review slice)",
            "a review run or a structured board error",
            match luna_proto::BoardError::from_rpc_message(&message) {
                Some(error) => Ok(json!({"skipped": error.code(), "reason": error.to_string()})),
                // A tool error is the board error's JSON text.
                None => serde_json::from_str::<Value>(&message)
                    .ok()
                    .filter(|error| error["code"].is_string())
                    .map(|error| json!({"skipped": error["code"], "reason": error["reason"]}))
                    .ok_or(message),
            },
        ),
    }
    match f
        .rpc_call(
            "BoardAuthoringStart",
            json!({"requestId":"authoring-1","projectId":"project-a"}),
        )
        .await
    {
        Ok(session) => {
            let session_id = session["id"].as_str().unwrap().to_owned();
            let context =
                CrossTaskBackend::board_context(&*f.rpc, &session_id).expect("authoring context");
            let (board, bytes) = board_catalog(&tools, &f.authority(&session_id));
            catalog_size("authoring", &board, bytes);
            evidence.check(
                "a ticket discussion gets only authoring tools and no chat role",
                "authoring audience, no role/task id, the authoring tool set",
                expect(
                    context.audience == luna_proto::BoardToolAudience::Authoring
                        && context.role.is_none()
                        && context.task_id.is_none()
                        && board == sorted(AUTHORING_TOOLS),
                    || format!("{context:?} {board:?}"),
                )
                .map(|_| json!(board)),
            );
            let saved = tools
                .invoke(
                    &f.authority(&session_id),
                    "board_create",
                    json!({"requestId":"authoring-create","title":"Drafted with AI"}),
                    &cancel,
                )
                .await;
            evidence.check(
                "a ticket discussion saves with authoring provenance",
                "origin authoring = the session",
                match saved {
                    Ok(receipt) => expect(
                        receipt["item"]["origin"]
                            == json!({"kind":"authoring","sessionId": session_id}),
                        || format!("{receipt}"),
                    )
                    .map(|_| receipt["key"].clone()),
                    Err(error) => Err(error.to_string()),
                },
            );
            let refused = tools
                .invoke(
                    &f.authority(&session_id),
                    "create_side_chat",
                    json!({"requestId":"authoring-side"}),
                    &cancel,
                )
                .await;
            evidence.check(
                "a ticket discussion cannot create side chats",
                "invalidRole",
                match refused {
                    Err(luna_engine::cross_task_tools::CrossTaskError::Backend(text))
                        if text.contains("\"code\":\"invalidRole\"") =>
                    {
                        Ok(json!("invalidRole"))
                    }
                    other => Err(format!("{other:?}")),
                },
            );
        }
        Err(error) => evidence.check(
            "authoring audience (authoring slice)",
            "a session or a structured board error",
            match error {
                luna_rpc::RpcError::Failed(message) => {
                    luna_proto::BoardError::from_rpc_message(&message)
                        .map(|error| json!({"skipped": error.code(), "reason": error.to_string()}))
                        .ok_or(message)
                }
                other => Err(other.to_string()),
            },
        ),
    }

    // ---- no orphan chats -----------------------------------------------
    let read = f.rpc_ok("BoardRead", json!({"workItemId": ticket})).await;
    let worker = read["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["role"] == "working" && link["active"] == true)
        .and_then(|link| link["taskId"].as_str())
        .map(str::to_owned);
    let mut allowed: HashSet<String> = chats_before.clone();
    allowed.insert(child.clone());
    for created in [&side_ctx_created, &sibling_created, &handoff_created]
        .into_iter()
        .flatten()
    {
        if let Some(task) = created["taskId"].as_str() {
            allowed.insert(task.into());
        }
    }
    if let Some(worker) = &worker {
        allowed.insert(worker.clone());
    }
    for task in [&reviewer_task, &review_worker].into_iter().flatten() {
        allowed.insert(task.clone());
    }
    let after = f.chat_ids();
    let unexpected: Vec<_> = after.difference(&allowed).cloned().collect();
    evidence.check(
        "board tools created no chat they did not report",
        "every new chat is a reported side chat, handoff, worker or the create_thread child",
        expect(
            unexpected.iter().all(|task| {
                f.core
                    .store
                    .is_board_authoring_session(task)
                    .unwrap_or(false)
            }),
            || format!("{unexpected:?}"),
        )
        .map(|_| json!({"before": chats_before.len(), "after": after.len()})),
    );
    if let Some(worker) = worker {
        let context = CrossTaskBackend::board_context(&*f.rpc, &worker).expect("worker context");
        evidence.check(
            "the ticket worker is a parent-locked side chat of the requesting main chat",
            "ticketWorker audience, side role, main parent = the main chat, parent locked",
            expect(
                context.audience == luna_proto::BoardToolAudience::TicketWorker
                    && context.main_parent_id.as_deref() == Some(main.as_str())
                    && context.parent_locked
                    && context
                        .ticket
                        .as_ref()
                        .is_some_and(|ticket| ticket.key == key),
                || format!("{context:?}"),
            )
            .map(|_| json!(context.ticket.map(|ticket| ticket.key))),
        );
    }
    evidence.write();
}
