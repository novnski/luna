//! CC-07: LUNA tools reach Claude over the shared per-process `luna` MCP
//! endpoint. The fake CLI (`fixtures/fake-claude-luna.py`) is the MCP client,
//! as the real 2.1.286 CLI is: it attaches from `--mcp-config`, resolves the
//! bearer token itself, emits 2.1.286 stream-json frames and sends
//! `_meta["claudecode/toolUseId"]` with each call.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{CancellationToken, ClaudeHarness, Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, LunaAgentRuntimeContract, RunRequest, RuntimeContext,
    RuntimeDelivery, RuntimeInstructionSection, RuntimeInstructionSource, RuntimeToolDefinition,
    SandboxLevel,
};

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-claude-luna.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn contract() -> LunaAgentRuntimeContract {
    LunaAgentRuntimeContract {
        version: 1,
        content_hash: "synthetic-claude-contract".into(),
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
            harness: HarnessId::ClaudeCode,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: Vec::new(),
        },
        environment: std::collections::BTreeMap::from([
            ("LUNA_THREAD_ID".into(), "thread-a".into()),
            ("LUNA_ENVIRONMENT_ID".into(), "environment-a".into()),
            ("LUNA_MACHINE_ID".into(), "machine-a".into()),
        ]),
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
            description: "Read this task's status.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            output_schema: serde_json::json!({"type":"object"}),
            required_scopes: Vec::new(),
            deadline_ms: 2_000,
            idempotency: "read-only".into(),
            presentation: "status".into(),
            activity_classification: "status".into(),
        }],
        omitted_tools: Vec::new(),
        delivery: RuntimeDelivery::ClaudeMcpTools,
    }
}

fn request(cwd: &std::path::Path) -> RunRequest {
    let mut model_options = serde_json::Map::new();
    model_options.insert(
        luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(contract()).unwrap(),
    );
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "scenario:luna-fence".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options,
        cwd: cwd.to_str().unwrap().into(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// The steering sender is returned so the caller keeps stdin open, as the
/// engine does for a warm Claude session.
fn controls(
    seen: Seen,
    prompts: Arc<Mutex<usize>>,
) -> (RunControls, mpsc::Sender<luna_harness::SteerMessage>) {
    let (steer_tx, steer_rx) = mpsc::channel(1);
    let controls = RunControls {
        request_input: Box::new(move |_| {
            // A LUNA tool must never reach the human approval bridge here.
            *prompts.lock().unwrap() += 1;
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: Some(Arc::new(move |invocation| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                if invocation.name != "luna_status" || invocation.arguments != serde_json::json!({})
                {
                    return Err(HarnessError::Protocol("unexpected tool request".into()));
                }
                seen.lock().unwrap().push((
                    invocation.context.provider_thread_id().to_owned(),
                    invocation.context.provider_turn_id().to_owned(),
                    invocation.context.provider_call_id().to_owned(),
                ));
                Ok(serde_json::json!({"threadId": "thread-a", "via": "engine-executor"}))
            })
        })),
    };
    (controls, steer_tx)
}

/// F1, F3, F5 (shape), F7–F14, F20, F22 from evidence/cc07-bridge/failure-cases.md.
#[tokio::test]
async fn claude_luna_tools_fence_matrix_over_shared_mcp_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let seen: Seen = Arc::default();
    let prompts = Arc::new(Mutex::new(0));
    let harness = ClaudeHarness::new().with_executable(fake());
    assert_eq!(
        harness.luna_runtime_delivery(),
        RuntimeDelivery::ClaudeMcpTools
    );
    let (run_controls, _steering) = controls(Arc::clone(&seen), Arc::clone(&prompts));
    let stream = harness
        .run(request(root.path()), run_controls)
        .await
        .expect("run starts");
    let events = tokio::time::timeout(
        Duration::from_secs(60),
        stream
            .map(|event| event.expect("stream event"))
            .collect::<Vec<_>>(),
    )
    .await
    .expect("fence matrix finished");
    let steps: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("luna-steps.json"))
            .unwrap_or_else(|error| panic!("fake wrote no steps ({error}); events: {events:#?}")),
    )
    .unwrap();
    std::fs::write(
        root.path().join("events.txt"),
        format!("{events:#?}\n{steps:#}"),
    )
    .unwrap();
    if let Ok(dir) = std::env::var("CC07_EVIDENCE_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            std::path::Path::new(&dir).join("harness-fence-steps.json"),
            format!("{steps:#}"),
        );
    }

    // Attachment: one inline `luna` server, exact tool allowlist, no strict
    // mode (the user's own MCP connections stay), token only in the env.
    let attachment = &steps["attachment"];
    assert_eq!(
        attachment["servers"],
        serde_json::json!(["luna"]),
        "{steps:#}"
    );
    assert!(
        attachment["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    assert_eq!(attachment["allowedTools"], "mcp__luna__luna_status");
    // Human local approvals must outlive the HTTP transport's 60s default.
    // Keep this scoped to LUNA instead of changing unrelated MCP connections.
    assert_eq!(attachment["timeout"], 86_400_000);
    assert_eq!(attachment["strict"], false);
    assert_eq!(attachment["headersHelper"], true);
    assert_eq!(attachment["tokenInEnv"], true);
    assert_eq!(attachment["tokenInArgv"], false);

    // 2.1.286 startup against the shared endpoint.
    let handshake = &steps["handshake"];
    assert_eq!(
        handshake["discover"],
        serde_json::json!([200, -32601]),
        "{steps:#}"
    );
    assert_eq!(handshake["initialize"][0], 200);
    assert_eq!(handshake["initialize"][2], "luna");
    assert_eq!(handshake["initialized"], 202);
    assert_eq!(handshake["sse"], 405);
    assert_eq!(handshake["tools"], serde_json::json!(["luna_status"]));
    assert_eq!(handshake["readOnly"]["luna_status"], true);
    assert_eq!(steps["wrongToken"], 401);

    let ok = |name: &str| {
        let step = &steps[name];
        assert_eq!(step["status"], 200, "{name}: {step}");
        assert_eq!(step["isError"], false, "{name}: {step}");
        assert!(
            step["text"].as_str().unwrap().contains("engine-executor"),
            "{name}: {step}"
        );
    };
    let refused = |name: &str, needle: &str| {
        let step = &steps[name];
        assert_eq!(step["status"], 200, "{name}: {step}");
        assert_eq!(step["isError"], true, "{name}: {step}");
        assert!(
            step["text"].as_str().unwrap().contains(needle),
            "{name}: {step}"
        );
    };
    ok("root");
    refused("replay", "repeated");
    refused("missingMeta", "call ID");
    refused("unattached", "not attached");
    refused("nameMismatch", "does not match");
    refused("subagent", "not attached");
    assert!(
        steps["subagent"]["seconds"].as_f64().unwrap() < 2.0,
        "{steps:#}"
    );
    ok("raced");
    refused("forged", "does not match");
    assert!(
        steps["forged"]["seconds"].as_f64().unwrap() >= 4.0,
        "{steps:#}"
    );
    assert_eq!(
        steps["preapproved"]["response"]["response"]["behavior"],
        "allow"
    );
    assert_eq!(steps["preapproved"]["response"]["request_id"], "perm-luna");
    refused("afterResult", "does not match");
    ok("wake");
    assert_eq!(
        *prompts.lock().unwrap(),
        0,
        "no human prompt for LUNA tools"
    );

    // Turn evidence: one minted turn per root `system:init` (after a result),
    // announced after SessionStarted, and the exact ids the executor saw.
    let session = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .expect("session started");
    let turns = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderTurnStarted { thread_id, turn_id } => {
                assert_eq!(thread_id, &session);
                Some(turn_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 2, "{events:#?}");
    assert_ne!(turns[0], turns[1]);
    let first_turn_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ProviderTurnStarted { .. }))
        .unwrap();
    let session_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::SessionStarted { .. }))
        .unwrap();
    assert!(session_index < first_turn_index);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            (session.clone(), turns[0].clone(), "toolu_A".into()),
            (session.clone(), turns[0].clone(), "toolu_R".into()),
            (session.clone(), turns[1].clone(), "toolu_W".into()),
        ]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    )));

    // Revocation: the endpoint and its token end with the process.
    let url = attachment["url"].as_str().unwrap().to_owned();
    let retired = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match reqwest::Client::new()
                .post(&url)
                .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .send()
                .await
            {
                Err(_) => break,
                Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await;
    assert!(
        retired.is_ok(),
        "endpoint still serving after the run ended"
    );
}

/// F6/F15: a contract that advertises tools without an engine executor is
/// refused instead of attaching unreachable tools.
#[tokio::test]
async fn claude_contract_with_tools_but_no_executor_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (mut controls, _steering) = controls(Arc::default(), Arc::default());
    controls.invoke_luna_tool = None;
    let error = ClaudeHarness::new()
        .with_executable(fake())
        .run(request(root.path()), controls)
        .await
        .err()
        .expect("refused");
    assert!(error.to_string().contains("unsupported tools"), "{error}");
}
