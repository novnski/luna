//! Failure-first subprocess E2E for provider MCP launch isolation.
//!
//! F1: metadata-only Claude probes must not load inherited MCP before init.
//! F2: QA Codex must disable an effective named MCP before thread/start.
//! F3: QA Claude must request strict MCP isolation on a normal run.
//! F4: ordinary Codex/Claude runs must leave user MCP enabled.
//! The real Codex override-merge probe and native app acceptance are recorded
//! separately under target/qa/agent-delegation-ux/mcp-investigation/.

#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::StreamExt;
use luna_harness::{
    CancellationToken, ClaudeHarness, CodexHarness, CommandScope, Harness, RunControls,
    SteerMessage,
};
use luna_proto::{
    HarnessId, LunaAgentRuntimeContract, PermissionMode, RunRequest, RuntimeContext,
    RuntimeDelivery, RuntimeInstructionSection, RuntimeInstructionSource, RuntimeToolDefinition,
    SandboxLevel,
};
use tokio::sync::{mpsc, oneshot};

fn peer(home: &Path, name: &str) -> PathBuf {
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-isolation-peer.py");
    let path = home.join(name);
    std::fs::copy(source, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn request(cwd: &Path) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "isolation E2E".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>) {
    let (steer, steering) = mpsc::channel(1);
    (
        RunControls {
            request_input: Box::new(|_| oneshot::channel().1),
            steering,
            interrupt: CancellationToken::new(),
            invoke_luna_tool: None,
        },
        steer,
    )
}

fn luna_contract(harness: HarnessId) -> LunaAgentRuntimeContract {
    LunaAgentRuntimeContract {
        version: 1,
        content_hash: "mcp-isolation-e2e".into(),
        context: RuntimeContext {
            app_version: "test".into(),
            protocol_version: 1,
            thread_id: "thread-a".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: None,
            project_name: None,
            environment_id: "environment-a".into(),
            machine_id: "machine-a".into(),
            machine_name: "machine-a".into(),
            workspace_root: "/synthetic".into(),
            worktree_id: None,
            binding_generation: Some(1),
            provider_profile_id: None,
            branch: None,
            harness,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: Vec::new(),
            chat: None,
        },
        environment: std::collections::BTreeMap::new(),
        local_endpoints: Vec::new(),
        instructions: vec![RuntimeInstructionSection {
            name: "LUNA core".into(),
            source: RuntimeInstructionSource::LunaCore,
            provenance: "synthetic fixture".into(),
            text: "Run inside LUNA.".into(),
        }],
        tools: vec![RuntimeToolDefinition {
            name: "luna_status".into(),
            version: 1,
            description: "Read task status".into(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            output_schema: serde_json::json!({"type":"object"}),
            required_scopes: Vec::new(),
            deadline_ms: 2_000,
            idempotency: "read-only".into(),
            presentation: "status".into(),
            activity_classification: "status".into(),
        }],
        omitted_tools: Vec::new(),
        delivery: match harness {
            HarnessId::ClaudeCode => RuntimeDelivery::ClaudeMcpTools,
            HarnessId::Codex => RuntimeDelivery::CodexMcpTools,
            _ => unreachable!(),
        },
    }
}

async fn run_once(harness: &dyn Harness, request: RunRequest) {
    let (controls, _steer) = controls();
    let stream = harness.run(request, controls).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("synthetic provider turn ended");
}

async fn run_with_luna(harness: &dyn Harness, mut request: RunRequest, id: HarnessId) {
    request.model_options.insert(
        luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(luna_contract(id)).unwrap(),
    );
    let (mut controls, _steer) = controls();
    controls.invoke_luna_tool = Some(Arc::new(|_| Box::pin(async { Ok(serde_json::json!({})) })));
    let stream = harness.run(request, controls).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("synthetic LUNA-tool provider turn ended");
}

fn launches(home: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(home.join("mcp-isolation-launches.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_and_qa_launches_keep_mcp_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let claude_home = temp.path().join("claude-home");
    let codex_home = temp.path().join("codex-home");
    let project = temp.path().join("project");
    for path in [&claude_home, &codex_home, &project] {
        std::fs::create_dir_all(path).unwrap();
    }
    let claude = ClaudeHarness::new()
        .with_executable(peer(temp.path(), "claude-peer.py"))
        .with_config_dir(&claude_home);
    let codex = CodexHarness::new().with_executable(peer(temp.path(), "codex-peer.py"));

    // Metadata probes are isolated even in a normal user app.
    claude.live_models().await.unwrap();
    claude
        .commands_in(&CommandScope {
            cwd: Some(project.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    claude
        .native_extensions(Some(&project), PermissionMode::Full, true)
        .await
        .unwrap();
    let metadata = launches(&claude_home);
    assert!(metadata.len() >= 3, "{metadata:?}");
    assert!(
        metadata.iter().all(
            |launch| launch["strict"] == true && launch["mcp_servers"] == serde_json::json!([])
        ),
        "{metadata:?}"
    );

    // Ordinary sessions preserve user MCP discovery.
    run_once(&claude, request(&project)).await;
    let mut ordinary_codex = request(&project);
    ordinary_codex.model_options.insert(
        luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        codex_home.to_string_lossy().into_owned().into(),
    );
    run_once(&codex, ordinary_codex).await;
    assert_eq!(launches(&claude_home).last().unwrap()["strict"], false);
    assert!(
        launches(&codex_home).last().unwrap()["disabled"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // This test binary has one test and a current-thread runtime. The flag is
    // process scoped, as it is in a signed QA bundle; restore it after both
    // provider subprocesses have exited.
    unsafe { std::env::set_var("LUNA_QA_MCP_ISOLATION", "1") };
    run_with_luna(&claude, request(&project), HarnessId::ClaudeCode).await;
    let mut qa_codex = request(&project);
    qa_codex.model_options.insert(
        luna_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        codex_home.to_string_lossy().into_owned().into(),
    );
    run_with_luna(&codex, qa_codex, HarnessId::Codex).await;
    unsafe { std::env::remove_var("LUNA_QA_MCP_ISOLATION") };

    let qa_claude = launches(&claude_home).last().unwrap().clone();
    assert_eq!(qa_claude["strict"], true, "{qa_claude:?}");
    assert_eq!(qa_claude["mcp_servers"], serde_json::json!(["luna"]));
    let codex_launches = launches(&codex_home);
    let qa_codex = codex_launches.last().unwrap();
    assert_eq!(
        qa_codex["disabled"],
        serde_json::json!(["mcp_servers.marker.enabled=false"]),
        "{codex_launches:?}"
    );
    assert_eq!(qa_codex["luna_enabled"], true, "{codex_launches:?}");
}
