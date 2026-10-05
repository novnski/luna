//! CC-13 E2E: the real `ClaudeHarness` on the fake CLI → engine run loop →
//! session document → `WatchNativeTasks` RPC. Native workflow updates keep
//! arriving after the launching turn's Done (parked session) and must persist
//! without reopening the turn. A provider process that exits mid-workflow
//! leaves no task looking live. Offline: no installed agent or account.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{
    HarnessId, NativeTaskEndReason, NativeTaskKind, NativeTaskLedger, NativeTaskStatus, RunRequest,
    SandboxLevel, SessionStatus,
};

fn assemble(dir: &Path) -> EngineCore {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o755));
    }
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fixture)));
    let profile = EngineProfile::development(dir, "test-org", "test-user");
    let accounts = AgentAccountsConfig {
        data_dir: dir.join("synthetic-provider-state"),
        claude_config_dir: dir.join("synthetic-claude"),
        claude_config_file: dir.join("synthetic-claude.json"),
        codex_home: dir.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    // Dispatch resolves Claude's provider-native account from this file.
    std::fs::write(
        &accounts.claude_config_file,
        r#"{"oauthAccount":{"accountUuid":"synthetic-claude-account","emailAddress":"synthetic@example.invalid"}}"#,
    )
    .unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile,
        registry,
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap()
}

fn request(dir: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: dir.display().to_string(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![],
        worktree: None,
        resume: None,
    }
}

fn ledger(core: &EngineCore, chat: &str) -> NativeTaskLedger {
    core.doc_host.open(chat).unwrap().doc().native_tasks()
}

fn entries(core: &EngineCore, chat: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap_or_default()
}

async fn run_until_idle(core: &EngineCore, chat: &str, prompt: &str, dir: &Path) {
    core.sessions
        .dispatch(
            chat,
            HarnessId::ClaudeCode,
            request(dir, prompt),
            Some("user-prompt".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let idle = core
                .sessions
                .session_status(chat)
                .is_some_and(|s| s.status == SessionStatus::Idle);
            // The run ends only when the fake CLI exits: no task may stay live.
            if idle && !ledger(core, chat).tasks.is_empty() && !ledger(core, chat).has_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("run ends with a settled native-task ledger");
}

#[tokio::test(flavor = "multi_thread")]
async fn parked_workflow_updates_persist_and_stream_without_reopening_the_turn() {
    const CHAT: &str = "claude-native-workflow";
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    run_until_idle(&core, CHAT, "scenario:workflow", dir.path()).await;

    let ledger = ledger(&core, CHAT);
    assert_eq!(
        ledger.tasks.len(),
        2,
        "legacy workflow ledger stays compatible; shells use an additive key"
    );
    let shells = core.doc_host.open(CHAT).unwrap().doc().native_shell_tasks();
    assert_eq!(shells.tasks.len(), 1);
    assert_eq!(shells.tasks[0].kind, NativeTaskKind::Shell);
    assert_eq!(shells.tasks[0].status, NativeTaskStatus::Completed);
    let workflow = &ledger.tasks[0];
    assert_eq!(
        (workflow.task_id.as_str(), workflow.kind),
        ("wf1", NativeTaskKind::Workflow)
    );
    assert_eq!(workflow.status, NativeTaskStatus::Completed);
    assert_eq!(workflow.name.as_deref(), Some("audit-routes"));
    assert_eq!(workflow.phases.len(), 2);
    assert_eq!(workflow.agents.len(), 3);
    assert_eq!(workflow.total_tokens, Some(3_400));
    assert!(workflow.output_file.is_some());
    let teammate = &ledger.tasks[1];
    assert_eq!(teammate.kind, NativeTaskKind::Teammate);
    assert_eq!(
        teammate.end_reason,
        Some(NativeTaskEndReason::WorkerRestart)
    );

    // Control-plane only: the Workflow call stays an ordinary tool chip, the
    // run never becomes a subagent document, and no task text leaks into the
    // transcript.
    let parent = entries(&core, CHAT);
    let workflow_chip = parent
        .iter()
        .flat_map(|e| &e.parts)
        .find_map(|p| match p {
            MessagePart::Tool {
                id, subagent_ref, ..
            } if id == "toolu_wf" => Some(subagent_ref.clone()),
            _ => None,
        })
        .expect("workflow tool chip");
    assert_eq!(workflow_chip, None);
    let assistant: String = parent
        .iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(assistant.contains("Started the audit workflow."));
    assert!(!assistant.contains("list routes") && !assistant.contains("blocked by safety"));

    // The UI's read path: the watch opens with the persisted ledger.
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut rx = client
        .subscribe(
            luna_rpc::methods::WATCH_NATIVE_TASKS,
            serde_json::json!({ "chatId": CHAT }),
        )
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .expect("opening ledger frame");
    let streamed: NativeTaskLedger = serde_json::from_value(first).unwrap();
    assert_eq!(streamed, ledger);
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_exit_mid_workflow_settles_it_as_session_ended() {
    const CHAT: &str = "claude-native-workflow-gone";
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    run_until_idle(&core, CHAT, "scenario:abandoned-workflow", dir.path()).await;
    let ledger = ledger(&core, CHAT);
    assert_eq!(ledger.tasks.len(), 1);
    let task = &ledger.tasks[0];
    assert_eq!(task.task_id, "wf2");
    assert_eq!(task.status, NativeTaskStatus::Stopped);
    assert_eq!(task.end_reason, Some(NativeTaskEndReason::SessionEnded));
    assert_eq!(task.agents.len(), 1, "last observed graph is kept");
}
