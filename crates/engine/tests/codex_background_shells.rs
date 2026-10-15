//! Issue #22: a preview server a Codex agent leaves running must outlive the
//! parked session's idle reaper, show as a live background shell, and record
//! its exit. Offline protocol peer (frames recorded from Codex 0.162.1) ->
//! real Codex adapter -> engine run loop -> session document. Not evidence of
//! a live provider or native UI.
#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_doc::MessagePart;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::CodexHarness;
use luna_proto::{
    HarnessId, NativeTaskKind, NativeTaskLedger, NativeTaskStatus, RunRequest, SandboxLevel,
    SessionStatus,
};

const CHAT: &str = "codex-background-preview";
/// Parked-session reaper window (process-global; this file's only test sets
/// it before the engine assembles).
const IDLE_MS: u64 = 300;

fn assemble(dir: &Path) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/codex-background-terminal.py"),
        ),
    ));
    let accounts = AgentAccountsConfig {
        data_dir: dir.join("synthetic-state"),
        claude_config_dir: dir.join("synthetic-claude"),
        claude_config_file: dir.join("synthetic-claude.json"),
        codex_home: dir.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(dir, "fixture-org", "fixture-user"),
        registry,
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap()
}

fn shells(core: &EngineCore) -> NativeTaskLedger {
    core.doc_host.open(CHAT).unwrap().doc().native_shell_tasks()
}

fn preview_chip(core: &EngineCore) -> Option<(bool, bool)> {
    let entries = core
        .doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .read_entries()
        .ok()?;
    entries.iter().flat_map(|e| &e.parts).find_map(|p| match p {
        MessagePart::Tool {
            id,
            resolved,
            is_error,
            ..
        } if id == "preview" => Some((*resolved, *is_error)),
        _ => None,
    })
}

async fn wait_for(what: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn parked_session_keeps_a_running_preview_and_records_its_exit() {
    // SAFETY: set before the engine (the only reader) exists; one test here.
    unsafe { std::env::set_var("LUNA_SESSION_IDLE_MS", IDLE_MS.to_string()) };
    let dir = tempfile::tempdir().unwrap();
    let terminated = dir.path().join("peer-terminated");
    let core = assemble(dir.path());
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::Codex,
            RunRequest {
                prompt: "Start the preview on 5174 and leave it running".into(),
                harness: None,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: dir.path().display().to_string(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: true,
                permission_mode: luna_proto::PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                attachments: vec![],
                input_items: Vec::new(),
                worktree: None,
                resume: None,
                review: None,
                goal: None,
            },
            Some("fixture-user".into()),
        )
        .await
        .unwrap();
    wait_for("the turn to park", || {
        core.sessions
            .session_status(CHAT)
            .is_some_and(|s| s.status == SessionStatus::Idle)
    })
    .await;

    // Several reaper windows pass with the preview still serving: the Codex
    // process that owns it must not be ended.
    tokio::time::sleep(Duration::from_millis(IDLE_MS * 5)).await;
    assert!(
        !terminated.exists(),
        "the idle reaper ended the Codex process that owns the running preview"
    );
    let ledger = shells(&core);
    let [preview] = ledger.tasks.as_slice() else {
        panic!("one background shell expected: {ledger:?}");
    };
    assert_eq!(preview.kind, NativeTaskKind::Shell);
    assert_eq!(preview.status, NativeTaskStatus::Running);
    assert_eq!(preview.description, "npm run preview -- --port 5174");
    assert_eq!(preview.tool_use_id.as_deref(), Some("preview"));
    assert_eq!(
        preview.provider_session_id.as_deref(),
        Some("background-thread")
    );
    assert_eq!(preview_chip(&core), Some((false, false)));

    // The preview process exits on its own, turns after it started: the exit
    // code reaches both the shell task and the launching chip.
    std::fs::write(dir.path().join("stop-preview"), b"").unwrap();
    wait_for("the preview exit to be recorded", || {
        shells(&core)
            .tasks
            .first()
            .is_some_and(|task| task.status == NativeTaskStatus::Failed)
            && preview_chip(&core) == Some((true, true))
    })
    .await;
    let exited = shells(&core).tasks.remove(0);
    assert_eq!(exited.summary.as_deref(), Some("Exited with code 143"));
    assert_eq!(exited.error.as_deref(), Some("Exited with code 143"));
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Idle),
        "a background exit must not reopen the parked turn"
    );

    // With nothing left running, the ordinary idle reaper releases Codex.
    wait_for("the idle reaper to release Codex", || terminated.exists()).await;
    assert_eq!(shells(&core).tasks, vec![exited]);
    core.shutdown().await;
}
