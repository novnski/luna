//! CC-07 engine E2E: a Claude chat calls LUNA tools through the shared
//! per-process `luna` MCP endpoint, the engine-built authority, the provider
//! turn fence and the durable backend. The deterministic fake CLI
//! (`harness/tests/fixtures/fake-claude-luna.py`) is the MCP client, as the
//! real 2.1.286 CLI is. One harmless read needs no approval; create_thread and
//! send_message_to_thread are each separately approved (and one send denied).

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{
    AgentEvent, ApprovalDecision, DoneStatus, HarnessId, PermissionMode, RunRequest, SandboxLevel,
    TaskLineage,
};
use tempfile::TempDir;

fn account_config(root: &Path) -> AgentAccountsConfig {
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
        serde_json::json!({
            "oauthAccount": {"accountUuid": "claude-fixture", "emailAddress": "claude@example.test"}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        config.claude_config_dir.join(".credentials.json"),
        serde_json::json!({"claudeAiOauth": {"accessToken": "synthetic"}}).to_string(),
    )
    .unwrap();
    config
}

fn fake() -> std::path::PathBuf {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude-luna.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn run_request(prompt: &str, cwd: &Path) -> RunRequest {
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
        sandbox: SandboxLevel::ReadOnly,
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

async fn read_results(dir: &Path, label: &str) -> serde_json::Value {
    let file = dir.join(format!(".luna-claude-results-{label}.json"));
    for _ in 0..400 {
        if let Ok(text) = std::fs::read_to_string(&file) {
            return serde_json::from_str(&text).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("fake Claude never wrote {}", file.display());
}

async fn wait_refused(url: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match reqwest::Client::new()
                .post(url)
                .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .send()
                .await
            {
                Err(_) => break,
                Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("retired endpoint stops serving");
}

fn step(results: &serde_json::Value, index: usize) -> (bool, &str) {
    let step = &results[index.to_string()];
    assert_eq!(step["status"], 200, "step {index}: {results:#}");
    (
        step["isError"].as_bool().expect("tool result"),
        step["text"].as_str().unwrap_or_default(),
    )
}

#[tokio::test]
async fn claude_chat_reads_creates_and_sends_through_luna_mcp_with_separate_approvals() {
    let root = TempDir::new().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake())));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(
            &root.path().join("engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        registry,
        HarnessId::ClaudeCode,
        None,
        account_config(root.path()),
    )
    .unwrap();
    let _rpc = core.rpc_service();
    // The CLI reports its physical cwd (`/private/var/...` on macOS); resume
    // matching compares it with the dispatched cwd.
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
    let source = uuid::Uuid::new_v4().to_string();
    let target = uuid::Uuid::new_v4().to_string();
    let foreign = uuid::Uuid::new_v4().to_string();
    // A Claude parent configuration: create_thread children inherit it.
    let config = luna_proto::ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::ReadOnly,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    };
    for (id, project, path) in [
        (&source, "project-a", &project_a),
        (&target, "project-a", &project_a),
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

    let plan = serde_json::json!([
        {"tool": "luna_status", "arguments": {}},
        {"tool": "list_threads", "arguments": {"limit": 20}},
        {"tool": "create_thread", "arguments": {
            "prompt": "Claude child brief", "idempotencyKey": "claude-child-1", "title": "Claude child"}},
        {"tool": "send_message_to_thread", "arguments": {
            "taskId": target, "message": "Claude queued follow-up", "mode": "queue",
            "idempotencyKey": "claude-send-1"}},
        {"tool": "send_message_to_thread", "arguments": {
            "taskId": target, "message": "Claude denied follow-up", "mode": "queue",
            "idempotencyKey": "claude-send-deny"}},
        {"tool": "read_thread", "arguments": {"taskId": foreign}},
    ]);
    std::fs::write(
        project_a.join(".luna-claude-plan-run1.json"),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();

    let (_, mut events) = core.sessions.subscribe(&source, 0).unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::ClaudeCode,
            run_request("scenario:luna-plan:run1", &project_a),
            Some("cc07-run1".into()),
        )
        .await
        .unwrap();
    let (approvals, status) = tokio::time::timeout(Duration::from_secs(60), async {
        let mut approvals = 0;
        loop {
            match events.recv().await.unwrap().event {
                AgentEvent::InputRequested {
                    request_id,
                    questions,
                } => {
                    approvals += 1;
                    assert_eq!(questions.len(), 1);
                    let question = &questions[0];
                    let approval = question.approval.as_ref().expect("LUNA approval card");
                    assert_eq!(approval.provider, HarnessId::ClaudeCode);
                    assert!(
                        approval
                            .turn_id
                            .as_deref()
                            .unwrap()
                            .starts_with("claude-turn-")
                    );
                    let (expected, decision, outcome) = match approvals {
                        1 => (
                            "Claude child brief",
                            ApprovalDecision::AllowOnce,
                            luna_engine::sessions::LunaApprovalOutcome::AllowedOnce,
                        ),
                        2 => (
                            "Claude queued follow-up",
                            ApprovalDecision::AllowOnce,
                            luna_engine::sessions::LunaApprovalOutcome::AllowedOnce,
                        ),
                        _ => (
                            "Claude denied follow-up",
                            ApprovalDecision::Deny,
                            luna_engine::sessions::LunaApprovalOutcome::Denied,
                        ),
                    };
                    assert!(
                        question.question.contains(expected),
                        "{}",
                        question.question
                    );
                    assert_eq!(
                        core.sessions.respond_luna_local_ui_approval(
                            &source,
                            &request_id,
                            decision
                        ),
                        outcome
                    );
                    // One approval authorises one call only.
                    assert_eq!(
                        core.sessions.respond_luna_local_ui_approval(
                            &source,
                            &request_id,
                            ApprovalDecision::AllowOnce
                        ),
                        luna_engine::sessions::LunaApprovalOutcome::Stale
                    );
                }
                AgentEvent::Done { status, .. } => break (approvals, status),
                _ => {}
            }
        }
    })
    .await
    .expect("Claude plan turn settled");
    assert_eq!(approvals, 3, "reads must not ask; each mutation asks once");
    assert_eq!(status, DoneStatus::Completed);

    let run1 = read_results(&project_a, "run1").await;
    if let Ok(dir) = std::env::var("CC07_EVIDENCE_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            Path::new(&dir).join("engine-run1-results.json"),
            format!("{run1:#}"),
        );
    }
    let attachment = &run1["attachment"];
    assert_eq!(
        attachment["servers"],
        serde_json::json!(["luna"]),
        "{run1:#}"
    );
    assert_eq!(attachment["strict"], false);
    assert_eq!(attachment["tokenInArgv"], false);
    let allowed = attachment["allowedTools"].as_str().unwrap();
    let tools = run1["handshake"]["tools"].as_array().unwrap();
    for name in [
        "luna_status",
        "list_threads",
        "read_thread",
        "wait_threads",
        "send_message_to_thread",
        "create_thread",
    ] {
        assert!(tools.iter().any(|tool| tool == name), "{name}: {run1:#}");
        assert!(
            allowed
                .split(',')
                .any(|tool| tool == format!("mcp__luna__{name}")),
            "{allowed}"
        );
    }

    // Harmless reads: no approval, engine-scoped results.
    let (failed, text) = step(&run1, 0);
    assert!(!failed && text.contains(&source), "{text}");
    let (failed, text) = step(&run1, 1);
    assert!(!failed, "{text}");
    assert!(text.contains(&target) && !text.contains(&foreign), "{text}");

    // create_thread: separately approved, durable lineage.
    let (failed, text) = step(&run1, 2);
    assert!(!failed, "{text}");
    let receipt: serde_json::Value = serde_json::from_str(text).unwrap();
    let child = receipt["taskId"].as_str().unwrap().to_owned();
    assert_eq!(receipt["parentTaskId"], source.as_str());
    assert_eq!(
        core.task_identity
            .view(&child)
            .unwrap()
            .task
            .lineage
            .parent_task_id
            .as_deref(),
        Some(source.as_str())
    );

    // send_message_to_thread: separately approved, durable queue receipt.
    let (failed, text) = step(&run1, 3);
    assert!(!failed, "{text}");
    let sent = core
        .doc_host
        .task_command_receipt("claude-send-1")
        .unwrap()
        .unwrap();
    assert_eq!(sent.status, luna_proto::CommandReceiptStatus::Applied);
    assert_eq!(sent.task_id, target);
    assert_eq!(
        core.doc_host
            .queued_message(&target, "claude-send-1")
            .unwrap()
            .unwrap()
            .text,
        "Claude queued follow-up"
    );
    // Denied send: an error for the model, no receipt, no queued row.
    let (failed, _) = step(&run1, 4);
    assert!(failed);
    assert!(
        core.doc_host
            .task_command_receipt("claude-send-deny")
            .unwrap()
            .is_none()
    );
    // Cross-project read is refused by the engine authority.
    let (failed, _) = step(&run1, 5);
    assert!(failed, "foreign-project read must be rejected");

    // Revocation: the process ended, so its endpoint and token are gone.
    let run1_url = attachment["url"].as_str().unwrap().to_owned();
    wait_refused(&run1_url).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while core.sessions.has_live_run(&source) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("run 1 retired");

    // Continuity on resume: a new process for the same native session gets a
    // new endpoint and token with the current tool set.
    std::fs::write(
        project_a.join(".luna-claude-plan-run2.json"),
        serde_json::to_vec(&serde_json::json!([{"tool": "luna_status", "arguments": {}}])).unwrap(),
    )
    .unwrap();
    core.sessions
        .dispatch(
            &source,
            HarnessId::ClaudeCode,
            run_request("scenario:luna-plan:run2", &project_a),
            Some("cc07-run2".into()),
        )
        .await
        .unwrap();
    let run2 = read_results(&project_a, "run2").await;
    if let Ok(dir) = std::env::var("CC07_EVIDENCE_DIR") {
        let _ = std::fs::write(
            Path::new(&dir).join("engine-run2-results.json"),
            format!("{run2:#}"),
        );
    }
    assert_eq!(
        run2["attachment"]["resume"], run1["attachment"]["session"],
        "{run2:#}"
    );
    assert_ne!(run2["attachment"]["url"], run1["attachment"]["url"]);
    let (failed, text) = step(&run2, 0);
    assert!(!failed && text.contains(&source), "{text}");
    core.shutdown().await;
}
