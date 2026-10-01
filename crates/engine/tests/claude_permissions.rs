//! CC-03 engine E2E: owner-queued run → real Claude adapter → deterministic
//! 2.1.286-shaped peer (`harness/tests/fixtures/claude-permissions-peer.py`)
//! → engine-owned pending approval in the doc → `RespondApproval` → exact
//! native control_response verified by the peer. No model, no provider.
//! Failure cases: `evidence/cc03-permissions/failure-cases.md` (A1–A3, G1–G2).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionCommandPayload};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{
    APPROVAL_NO_EXPIRY, ApprovalDecision, HarnessId, RunRequest, SandboxLevel, SessionStatus,
    UserInputQuestion,
};

const CHAT: &str = "claude-permissions-e2e";

fn assemble(root: &Path) -> EngineCore {
    let peer = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/claude-permissions-peer.py");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&peer, std::fs::Permissions::from_mode(0o755));
    }
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(peer)));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    // A synthetic provider-native identity (no credentials are used: the CLI
    // is the deterministic peer).
    std::fs::create_dir_all(&accounts.claude_config_dir).unwrap();
    std::fs::write(
        &accounts.claude_config_file,
        serde_json::json!({
            "oauthAccount": {"accountUuid": "cc03-fixture", "emailAddress": "cc03@example.test"}
        })
        .to_string(),
    )
    .unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "synthetic-org", "synthetic-user"),
        registry,
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap()
}

fn run_request(prompt: String) -> RunRequest {
    RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt,
        harness: Some(HarnessId::ClaudeCode),
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
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

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn parts(core: &EngineCore) -> Vec<MessagePart> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|entry| entry.parts)
        .collect()
}

/// The (request id, questions, resolved) of the first input part.
fn input(core: &EngineCore) -> Option<(String, Vec<UserInputQuestion>, bool)> {
    parts(core).into_iter().find_map(|part| match part {
        MessagePart::Input {
            request_id,
            questions,
            resolved,
            ..
        } => Some((request_id, questions, resolved)),
        _ => None,
    })
}

fn completed_with(core: &EngineCore, text: &str) -> bool {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .any(|entry| {
            entry.role == MessageRole::Assistant
                && entry.status == Some(MessageStatus::Complete)
                && entry.parts.iter().any(
                    |part| matches!(part, MessagePart::Text { text: t, .. } if t.contains(text)),
                )
        })
}

fn start(core: &EngineCore, scenario: &str, log: &Path) {
    let handle = core.doc_host.open(CHAT).unwrap();
    core.doc_host
        .queue_command_with_id_and_transfers(
            &handle.doc().chat_id().unwrap(),
            &format!("run-{scenario}"),
            SessionCommandPayload::Run {
                request: run_request(format!("scenario:{scenario} log:{}", log.display())),
                message_id: format!("user-{scenario}"),
            },
            Vec::new(),
        )
        .expect("admit owner run");
}

fn peer_fails(log: &Path) -> serde_json::Value {
    std::fs::read(log)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(|log| log["fails"].clone())
        .unwrap_or(serde_json::Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_approval_details_persist_and_session_choice_reaches_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("engine-approval.json");
    let core = assemble(dir.path());
    start(&core, "engine-approval", &log);

    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingApproval)
                && input(&core).is_some_and(|(_, _, resolved)| !resolved)
        },
        "Claude approval card",
    )
    .await;
    let (request_id, questions, _) = input(&core).unwrap();
    let approval = questions[0].approval.clone().expect("typed approval");
    assert_eq!(approval.provider, HarnessId::ClaudeCode);
    assert_eq!(approval.expires_at_ms, APPROVAL_NO_EXPIRY);
    let details = approval.details.expect("details persisted in the doc");
    assert_eq!(
        details.command.as_deref(),
        Some("npm test -- --watch=false")
    );
    assert_eq!(details.cwd.as_deref(), Some("/tmp/cc03-peer"));
    assert_eq!(
        questions[0].options,
        ["Deny", "Allow once", "Allow for session", "Cancel turn"]
    );
    // A forged choice index never resolves the request.
    assert!(
        !core
            .sessions
            .respond_approval(
                CHAT,
                &request_id,
                ApprovalDecision::ProviderChoice { index: 9 }
            )
            .unwrap()
    );
    // The legacy "allow for task" decision selects the exact session choice.
    let handle = core.doc_host.open(CHAT).unwrap();
    core.doc_host
        .queue_command_with_id_and_transfers(
            &handle.doc().chat_id().unwrap(),
            "approve-session",
            SessionCommandPayload::RespondApproval {
                request_id,
                decision: ApprovalDecision::AllowTask,
            },
            Vec::new(),
        )
        .expect("admit approval");
    wait_for(
        || completed_with(&core, "engine-approval"),
        "completed Claude turn",
    )
    .await;
    assert!(
        completed_with(&core, "peer-verified:engine-approval"),
        "peer fails: {}",
        peer_fails(&log)
    );
    assert!(input(&core).is_some_and(|(_, _, resolved)| resolved));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_native_cancel_retires_the_card_and_never_answers() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("engine-cancel.json");
    let core = assemble(dir.path());
    start(&core, "engine-cancel", &log);

    wait_for(
        || input(&core).is_some_and(|(_, _, resolved)| !resolved),
        "Claude approval card",
    )
    .await;
    let (request_id, _, _) = input(&core).unwrap();
    // The CLI withdraws the request; the engine retires the card before the
    // turn ends, and a late answer is refused.
    wait_for(
        || input(&core).is_some_and(|(_, _, resolved)| resolved),
        "card retired after native cancel",
    )
    .await;
    assert!(
        !completed_with(&core, "engine-cancel"),
        "retired before the turn ended"
    );
    assert!(
        !core
            .sessions
            .respond_approval(CHAT, &request_id, ApprovalDecision::AllowOnce)
            .unwrap()
    );
    wait_for(
        || completed_with(&core, "engine-cancel"),
        "completed Claude turn",
    )
    .await;
    assert!(
        completed_with(&core, "peer-verified:engine-cancel"),
        "peer fails: {}",
        peer_fails(&log)
    );
    core.shutdown().await;
}
