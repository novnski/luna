//! One-time local→synced profile import: correctness, chat2 lineage of
//! imported docs, journal carry-over, idempotence, the uploads read-root
//! marker, and (SW-11 E14) imported Codex/Claude past work staying attributed
//! in recorded-content search and handing off into a fresh task.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry};

use luna_engine::local_import::{ImportEvent, marker_grants_read_root};
use luna_engine::run_journal::journal_paths;
use luna_engine::{
    AgentAccountsConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry, RunJournal,
    default_registry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    ActivityEnvelope, ActivityKind, AgentEvent, ChatConfig, ConversationSourceContext,
    HistorySearchPage, PermissionMode, SandboxLevel,
};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};

fn assemble(profile: EngineProfile) -> EngineCore {
    EngineCore::assemble_with_profile(profile, Arc::new(default_registry()), HarnessId::Mock, None)
        .expect("assemble profile")
}

/// Seed a local profile with a space, two chats (one with a doc + journal,
/// one row-only), exactly as a local-first stretch leaves them on disk.
async fn seed_local(data_dir: &std::path::Path) -> (String, String, String) {
    let local = assemble(EngineProfile::local(data_dir).expect("local profile"));
    let device = local.device_id.clone();

    local
        .workspace
        .create_space(
            "space-1",
            &device,
            "/tmp/proj",
            Some("Project".into()),
            false,
        )
        .expect("create space");
    local
        .workspace
        .create_chat("chat-doc", Some("space-1"), None, None, None)
        .expect("create chat with doc");
    local
        .workspace
        .rename_chat("chat-doc", "Fix the flaky test")
        .expect("title");
    local
        .workspace
        .create_chat("chat-bare", Some("space-1"), None, None, None)
        .expect("create bare chat");

    // A real transcript doc, saved the way the local runtime saves it.
    let doc = SessionDoc::init("chat-doc").expect("init doc");
    doc.push_message(&SessionMessageEntry {
        id: "m1".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "t1".into(),
            text: "hello from local mode".into(),
        }],
        created_at: 1_700_000_000_000,
        device_id: device.clone(),
        status: None,
        continuation_of: None,
    })
    .expect("push message");
    let bytes = doc.export_snapshot().expect("snapshot");
    let store = luna_sync::DocsStore::open(data_dir.join("profiles").join("local"))
        .expect("open local store");
    store
        .save_snapshot_with_cursor("chat-doc", &bytes, 0, 2)
        .expect("save doc");
    drop(store);

    // Journal + resume budget as a run leaves them.
    let journals = data_dir.join("profiles").join("local").join("journals");
    std::fs::create_dir_all(&journals).expect("journals dir");
    let (journal, resume) = journal_paths(&journals, "chat-doc");
    std::fs::write(&journal, "{\"seq\":1,\"event\":{}}\n").expect("journal");
    std::fs::write(&resume, "1").expect("resume");

    // A committed attachment under the local uploads root.
    let uploads = data_dir.join("profiles").join("local").join("uploads");
    std::fs::create_dir_all(&uploads).expect("uploads dir");
    std::fs::write(uploads.join("ab12cd34-shot.png"), b"png").expect("attachment");

    local.shutdown().await;
    (device, "chat-doc".into(), "chat-bare".into())
}

fn run_import(core: &EngineCore) -> Vec<ImportEvent> {
    let importer = core.local_import.clone().expect("synced runtime importer");
    let mut events = Vec::new();
    importer.run(|event| events.push(event)).expect("import");
    events
}

fn summary(events: &[ImportEvent]) -> (usize, usize, usize, usize) {
    match events.last().expect("summary event") {
        ImportEvent::Summary {
            imported_chats,
            imported_spaces,
            skipped_chats,
            skipped_spaces,
            errors,
            ..
        } => {
            assert!(errors.is_empty(), "import errors: {errors:?}");
            (
                *imported_chats,
                *imported_spaces,
                *skipped_chats,
                *skipped_spaces,
            )
        }
        other => panic!("last event must be a summary, got {other:?}"),
    }
}

#[tokio::test]
async fn local_work_imports_into_synced_profile_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_device, chat_doc, chat_bare) = seed_local(dir.path()).await;

    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));

    let status = synced
        .local_import
        .as_ref()
        .expect("importer")
        .status()
        .expect("status");
    assert_eq!(status.available_chats, 2);
    assert_eq!(status.available_spaces, 1);
    assert!(!status.imported_before);

    let events = run_import(&synced);
    let (imported_chats, imported_spaces, skipped_chats, skipped_spaces) = summary(&events);
    assert_eq!((imported_chats, imported_spaces), (2, 1));
    assert_eq!((skipped_chats, skipped_spaces), (0, 0));

    // Rows landed with chat2 lineage and their content intact.
    let row = synced
        .workspace
        .chat(&chat_doc)
        .expect("read chat")
        .expect("imported chat row");
    assert_eq!(row.title.as_deref(), Some("Fix the flaky test"));
    assert_eq!(row.room_gen, Some(2), "imported chats must be chat2-born");
    assert_eq!(row.space_id.as_deref(), Some("space-1"));
    assert!(
        synced
            .workspace
            .chat(&chat_bare)
            .expect("read bare")
            .is_some(),
        "row-only chats import too"
    );
    assert!(
        synced
            .workspace
            .space("space-1")
            .expect("read space")
            .is_some()
    );

    // Doc bytes present in the synced store in born-chat2 shape (cursor 0,
    // epoch 2) — the shape DocHost pushes from VV zero on first room join.
    let store = luna_sync::DocsStore::open(dir.path().join("orgs").join("org1").join("user1"))
        .expect("open synced store");
    let (bytes, cursor, epoch) = store
        .load_snapshot_with_cursor(&chat_doc)
        .expect("load")
        .expect("imported doc row");
    assert_eq!((cursor, epoch), (0, 2));
    let source_store = luna_sync::DocsStore::open(dir.path().join("profiles").join("local"))
        .expect("open source store");
    assert_eq!(
        source_store
            .load_snapshot(&chat_doc)
            .expect("source load")
            .expect("source doc bytes"),
        bytes,
        "imported doc bytes are a verbatim copy of the local snapshot"
    );
    drop((store, source_store));

    // Journal + resume budget carried over.
    let (journal, resume) = journal_paths(
        &dir.path()
            .join("orgs")
            .join("org1")
            .join("user1")
            .join("journals"),
        &chat_doc,
    );
    assert!(journal.is_file(), "journal copied");
    assert!(resume.is_file(), "resume budget copied");

    // Marker recorded, and it grants the local uploads root read-only.
    let granted = marker_grants_read_root(dir.path(), "org1", "user1")
        .expect("marker grants local uploads root");
    assert_eq!(
        granted,
        dir.path().join("profiles").join("local").join("uploads")
    );
    assert!(
        marker_grants_read_root(dir.path(), "org1", "other-user").is_none(),
        "grants are per (org, user)"
    );

    // Second run: structural idempotence — everything is skipped.
    let status = synced
        .local_import
        .as_ref()
        .expect("importer")
        .status()
        .expect("status");
    assert_eq!(status.available_chats, 0);
    assert!(status.imported_before);
    let events = run_import(&synced);
    let (imported_chats, imported_spaces, skipped_chats, skipped_spaces) = summary(&events);
    assert_eq!((imported_chats, imported_spaces), (0, 0));
    assert_eq!((skipped_chats, skipped_spaces), (2, 1));

    synced.shutdown().await;
}

#[tokio::test]
async fn import_without_local_profile_is_a_clean_no_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));

    let status = synced
        .local_import
        .as_ref()
        .expect("importer")
        .status()
        .expect("status");
    assert_eq!((status.available_chats, status.available_spaces), (0, 0));

    let events = run_import(&synced);
    let (imported_chats, imported_spaces, ..) = summary(&events);
    assert_eq!((imported_chats, imported_spaces), (0, 0));

    synced.shutdown().await;
}

/// Read the summary WITHOUT asserting a clean run (the failure-injection tests
/// need the errors).
fn raw_summary(events: &[ImportEvent]) -> (usize, usize, Vec<String>) {
    match events.last().expect("summary event") {
        ImportEvent::Summary {
            imported_chats,
            skipped_chats,
            errors,
            ..
        } => (*imported_chats, *skipped_chats, errors.clone()),
        other => panic!("last event must be a summary, got {other:?}"),
    }
}

#[tokio::test]
async fn per_item_failures_surface_in_the_summary_and_leave_the_row_retryable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_device, chat_doc, chat_bare) = seed_local(dir.path()).await;

    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));

    // Injected failure: the target journals DIRECTORY is replaced by a file,
    // so the journal copy fails for the one chat that has a journal
    // (chat-doc); the journal-less chat-bare is untouched.
    let target_journals = dir
        .path()
        .join("orgs")
        .join("org1")
        .join("user1")
        .join("journals");
    std::fs::remove_dir_all(&target_journals).expect("clear journals dir");
    std::fs::write(&target_journals, b"obstruction").expect("plant journals obstruction");

    let events = run_import(&synced);
    let (imported, _skipped, errors) = raw_summary(&events);
    assert_eq!(imported, 1, "the healthy chat still imports");
    assert_eq!(
        errors.len(),
        1,
        "the failure is reported, not swallowed: {errors:?}"
    );
    assert!(errors[0].contains(&chat_doc), "{errors:?}");

    // The failed chat's ROW must not exist — structural idempotence makes the
    // retry pick it up again instead of skipping a half-imported chat.
    assert!(
        synced
            .workspace
            .chat(&chat_doc)
            .expect("read chat")
            .is_none(),
        "failed chat must stay pending for retry"
    );
    assert!(
        synced
            .workspace
            .chat(&chat_bare)
            .expect("read chat")
            .is_some()
    );

    // Retry after clearing the obstruction: only the failed chat imports.
    std::fs::remove_file(&target_journals).expect("clear obstruction");
    let events = run_import(&synced);
    let (imported, skipped, errors) = raw_summary(&events);
    assert!(errors.is_empty(), "retry is clean: {errors:?}");
    assert_eq!((imported, skipped), (1, 1));

    synced.shutdown().await;
}

#[tokio::test]
async fn corrupt_marker_is_moved_aside_not_clobbered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (..) = seed_local(dir.path()).await;

    std::fs::write(dir.path().join("local-import.json"), b"{ not json").expect("plant corrupt");

    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));
    let events = run_import(&synced);
    let (_, _, errors) = raw_summary(&events);
    assert!(
        errors.is_empty(),
        "a corrupt OLD marker fails nothing: {errors:?}"
    );

    // Evidence preserved, fresh marker valid, grant re-armed.
    assert!(
        dir.path().join("local-import.json.corrupt").is_file(),
        "corrupt bytes moved aside"
    );
    assert!(
        marker_grants_read_root(dir.path(), "org1", "user1").is_some(),
        "rebuilt marker grants the uploads root"
    );
    synced.shutdown().await;
}

#[tokio::test]
async fn marker_persistence_failure_is_an_import_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (..) = seed_local(dir.path()).await;

    // The marker path itself is a directory: the atomic rename must fail and
    // the failure must reach the summary — a grant that only lives in memory
    // breaks imported attachments on the next restart.
    std::fs::create_dir_all(dir.path().join("local-import.json")).expect("plant marker dir");

    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));
    let events = run_import(&synced);
    let (imported, _, errors) = raw_summary(&events);
    assert_eq!(imported, 2, "data still imports");
    assert!(
        errors.iter().any(|e| e.contains("import marker")),
        "marker failure must be reported: {errors:?}"
    );
    synced.shutdown().await;
}

#[tokio::test]
async fn spaces_only_profile_imports_its_spaces() {
    let dir = tempfile::tempdir().expect("tempdir");
    let local = assemble(EngineProfile::local(dir.path()).expect("local profile"));
    let device = local.device_id.clone();
    local
        .workspace
        .create_space(
            "space-only",
            &device,
            "/tmp/proj",
            Some("Project".into()),
            false,
        )
        .expect("create space");
    local.shutdown().await;
    drop(local);

    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));
    let status = synced
        .local_import
        .as_ref()
        .expect("importer")
        .status()
        .expect("status");
    assert_eq!((status.available_chats, status.available_spaces), (0, 1));

    let events = run_import(&synced);
    match events.last().expect("summary") {
        ImportEvent::Summary {
            imported_spaces,
            errors,
            ..
        } => {
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(*imported_spaces, 1);
        }
        other => panic!("expected summary, got {other:?}"),
    }
    assert!(
        synced
            .workspace
            .space("space-only")
            .expect("read space")
            .is_some()
    );
    synced.shutdown().await;
}

#[tokio::test]
async fn marker_keeps_one_entry_per_account() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (..) = seed_local(dir.path()).await;

    let first = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));
    run_import(&first);
    first.shutdown().await;
    drop(first);

    // A second account on the same device imports too; its marker entry must
    // not erase the first account's grant.
    let second = assemble(EngineProfile::synced(dir.path(), "org2", "user2"));
    run_import(&second);
    second.shutdown().await;

    assert!(marker_grants_read_root(dir.path(), "org1", "user1").is_some());
    assert!(marker_grants_read_root(dir.path(), "org2", "user2").is_some());
}

#[tokio::test]
async fn later_local_work_imports_as_a_delta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (device, ..) = seed_local(dir.path()).await;

    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));
    let events = run_import(&synced);
    assert_eq!(summary(&events).0, 2);
    synced.shutdown().await;
    drop(synced);

    // A later signed-out stretch creates one more local chat…
    let local = assemble(EngineProfile::local(dir.path()).expect("local profile"));
    local
        .workspace
        .create_chat("chat-later", None, Some(&device), None, Some("/tmp".into()))
        .expect("create later chat");
    local.shutdown().await;
    drop(local);

    // …and signing back in imports exactly that delta.
    let synced = assemble(EngineProfile::synced(dir.path(), "org1", "user1"));
    let status = synced
        .local_import
        .as_ref()
        .expect("importer")
        .status()
        .expect("status");
    assert_eq!(status.available_chats, 1);
    assert!(status.imported_before);
    let events = run_import(&synced);
    let (imported_chats, _, skipped_chats, _) = summary(&events);
    assert_eq!(imported_chats, 1);
    assert_eq!(skipped_chats, 2);
    synced.shutdown().await;
}

// ---------------------------------------------------------------------------
// SW-11 E14: past Codex and Claude work through the existing local import
// ---------------------------------------------------------------------------

const PAST_SPACE: &str = "past-work";
const CODEX_TASK: &str = "codex-past";
const CLAUDE_TASK: &str = "claude-past";
/// 2026-03-12T12:00:00Z and 2026-03-11T12:00:00Z.
const CODEX_ANSWER_AT: i64 = 1_773_316_800_000;
const CLAUDE_ANSWER_AT: i64 = 1_773_230_400_000;

/// Hermetic assembly: a mock-only registry and synthetic provider homes, so
/// the maintenance this test starts (history indexing) can never discover or
/// launch an installed provider CLI or read the user's provider state.
fn hermetic(profile: EngineProfile, root: &Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness { script: Vec::new() }));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile,
        Arc::new(registry),
        HarnessId::Mock,
        None,
        accounts,
    )
    .expect("assemble hermetic profile")
}

fn past_config(harness: HarnessId, model: &str, mode: PermissionMode) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: mode,
        effective_permission_mode: mode,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

/// A local profile that ran one Codex and one Claude task in a plain (not
/// Git) project folder, left exactly as those runs leave it: registry rows
/// with their provider settings, archive state and run-start context,
/// transcript snapshots, and journaled session metadata. Returns the rows as
/// the local profile held them.
async fn seed_past_work(data_dir: &Path, folder: &Path) -> (luna_proto::Chat, luna_proto::Chat) {
    let local = hermetic(
        EngineProfile::local(data_dir).expect("local profile"),
        data_dir,
    );
    let device = local.device_id.clone();
    let folder = folder.to_str().expect("utf-8 folder").to_string();
    let workspace = &local.workspace;
    workspace
        .create_space(
            PAST_SPACE,
            &device,
            &folder,
            Some("Past work".into()),
            false,
        )
        .expect("plain folder project");
    workspace
        .create_chat(
            CODEX_TASK,
            Some(PAST_SPACE),
            None,
            Some(past_config(
                HarnessId::Codex,
                "gpt-fixture",
                PermissionMode::Full,
            )),
            Some(folder.clone()),
        )
        .expect("codex task");
    workspace
        .rename_chat(CODEX_TASK, "Migrated retry investigation")
        .expect("codex title");
    workspace
        .set_chat_source_context(
            CODEX_TASK,
            &ConversationSourceContext {
                checkout_id: "past-checkout".into(),
                repo_root: folder.clone(),
                cwd: folder.clone(),
                branch: "feature/merged-retry".into(),
                head_sha: Some("0123456789abcdef0123456789abcdef01234567".into()),
                observed_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                    CODEX_ANSWER_AT - 30_000,
                )
                .unwrap(),
            },
        )
        .expect("run-start context");
    workspace.set_chat_harness_session(CODEX_TASK, "codex-native-session", &folder);
    workspace
        .set_chat_activity(
            CODEX_TASK,
            Some(CODEX_ANSWER_AT),
            Some(CODEX_ANSWER_AT - 60_000),
        )
        .expect("codex activity");
    workspace
        .set_chat_archived(CODEX_TASK, true)
        .expect("archive codex task");
    workspace
        .create_chat(
            CLAUDE_TASK,
            Some(PAST_SPACE),
            None,
            Some(past_config(
                HarnessId::ClaudeCode,
                "claude-fixture",
                PermissionMode::Auto,
            )),
            Some(folder.clone()),
        )
        .expect("claude task");
    workspace
        .rename_chat(CLAUDE_TASK, "Flaky login follow-up")
        .expect("claude title");
    workspace
        .set_chat_activity(
            CLAUDE_TASK,
            Some(CLAUDE_ANSWER_AT),
            Some(CLAUDE_ANSWER_AT - 60_000),
        )
        .expect("claude activity");

    let store = luna_sync::DocsStore::open(data_dir.join("profiles").join("local"))
        .expect("open local store");
    let journal = RunJournal::open(data_dir.join("profiles").join("local").join("journals"))
        .expect("open local journals");
    for (task, harness, model, prompt, answer, at) in [
        (
            CODEX_TASK,
            HarnessId::Codex,
            "gpt-fixture",
            "Why does the importer retry twice?",
            "Retry budget doubled after the migration; the second attempt reuses the cached session.",
            CODEX_ANSWER_AT,
        ),
        (
            CLAUDE_TASK,
            HarnessId::ClaudeCode,
            "claude-fixture",
            "Is the login test still flaky?",
            "Flaky login traced to the clock skew fixture; the suite now pins its timezone.",
            CLAUDE_ANSWER_AT,
        ),
    ] {
        let doc = SessionDoc::init(task).expect("init doc");
        for (id, role, text, created_at) in [
            (
                format!("{task}-prompt"),
                MessageRole::User,
                prompt,
                at - 30_000,
            ),
            (format!("{task}-answer"), MessageRole::Assistant, answer, at),
        ] {
            doc.push_message(&SessionMessageEntry {
                id,
                role,
                parts: vec![MessagePart::Text {
                    id: "text".into(),
                    text: text.into(),
                }],
                created_at,
                device_id: device.clone(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            })
            .expect("push message");
        }
        store
            .save_snapshot_with_cursor(task, &doc.export_snapshot().expect("snapshot"), 0, 2)
            .expect("save doc");
        journal
            .append(
                task,
                &AgentEvent::SessionStarted {
                    harness,
                    model: model.into(),
                    tools: Vec::new(),
                    cwd: folder.clone(),
                    session_id: format!("{task}-native"),
                    assistant_message_id: format!("{task}-answer"),
                },
            )
            .expect("journal session metadata");
    }
    drop((store, journal));
    let rows = (
        workspace.chat(CODEX_TASK).unwrap().expect("codex row"),
        workspace.chat(CLAUDE_TASK).unwrap().expect("claude row"),
    );
    local.shutdown().await;
    rows
}

/// Poll the production index (the engine's own bootstrap ingests imported
/// rows) for the answer `query` finds in `task`.
async fn indexed_answer(client: &RpcClient, query: &str, task: &str) -> ActivityEnvelope {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let page: HistorySearchPage = client
            .call_as(
                methods::SEARCH_SESSION_HISTORY,
                json!({"query": query, "scope": "global", "limit": 100}),
            )
            .await
            .expect("search session history");
        if let Some(event) = page
            .events
            .into_iter()
            .find(|event| event.task_id == task && event.kind == ActivityKind::Answer)
        {
            return event;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "imported history for {task} never became searchable for {query:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn execute(client: &RpcClient, task: &str, command_id: &str, intent: Value) -> Value {
    let basis: Value = client
        .call_as("GetTaskCommandSnapshot", json!({"taskId": task}))
        .await
        .expect("command snapshot");
    client
        .call_as(
            "ExecuteTaskCommand",
            json!({
                "taskId": task,
                "commandId": command_id,
                "actorId": "sw11-user",
                "clientId": "sw11-past-work",
                "clientSessionId": "sw11-past-work",
                "expectedTaskRevision": basis["snapshot"]["taskRevision"],
                "expectedBindingGeneration": basis["snapshot"]["bindingGeneration"],
                "intent": intent,
            }),
        )
        .await
        .expect("execute task command")
}

/// Behavior: Codex and Claude work recorded in the local profile keeps its
/// provider/model, timestamps, project, archive state and run-start context
/// through the existing local import, is found by global recorded-content
/// search with that attribution, and an explicit "new task with context"
/// from the archived Codex task creates a fresh task in the same host,
/// project and folder that resumes no provider session, runs with the
/// settings chosen for it and does not claim the source's merged branch.
/// Regressions caught: rows published before journals (provider/model lost
/// from search), archive/time/context dropped on import, imported history
/// invisible to search, and a fresh handoff that inherits the old provider
/// session, archive flag or run-start branch/PR context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_codex_and_claude_history_stays_attributed_and_hands_off_fresh() {
    let dir = tempfile::tempdir().expect("tempdir");
    let folder = dir.path().join("plain folder");
    std::fs::create_dir_all(&folder).expect("plain folder");
    let (codex_before, claude_before) = seed_past_work(dir.path(), &folder).await;
    assert!(codex_before.archived && !claude_before.archived);

    let synced = hermetic(
        EngineProfile::synced(dir.path(), "org1", "user1"),
        dir.path(),
    );
    let client = luna_rpc::memory_client(synced.rpc_service());
    let (imported_chats, imported_spaces, ..) = summary(&run_import(&synced));
    assert_eq!((imported_chats, imported_spaces), (2, 1));

    for before in [&codex_before, &claude_before] {
        let after = synced
            .workspace
            .chat(&before.id)
            .unwrap()
            .expect("imported row");
        assert_eq!(after.title, before.title);
        assert_eq!(after.config, before.config, "provider settings retained");
        assert_eq!(after.created_at, before.created_at);
        assert_eq!(after.last_message_at, before.last_message_at);
        assert_eq!(after.archived, before.archived);
        assert_eq!(after.source_context, before.source_context);
        assert_eq!(after.space_id.as_deref(), Some(PAST_SPACE));
        assert_eq!(after.device_id, synced.device_id, "owned by this host");
    }

    for (task, query, harness, model, at) in [
        (
            CODEX_TASK,
            "retry budget doubled",
            HarnessId::Codex,
            "gpt-fixture",
            CODEX_ANSWER_AT,
        ),
        (
            CLAUDE_TASK,
            "flaky login traced",
            HarnessId::ClaudeCode,
            "claude-fixture",
            CLAUDE_ANSWER_AT,
        ),
    ] {
        let event = indexed_answer(&client, query, task).await;
        assert_eq!(event.harness, Some(harness), "{task} provider from its run");
        assert_eq!(event.model.as_deref(), Some(model));
        assert_eq!(event.timestamp_ms, at, "{task} keeps its recorded time");
        assert_eq!(event.project_id.as_deref(), Some(PAST_SPACE));
        assert!(event.source.available_here);
        assert_eq!(event.source.message_id, format!("{task}-answer"));
    }

    let fresh = uuid::Uuid::new_v4().to_string();
    let request = json!({
        "kind": "portableFork",
        "sourceTaskId": CODEX_TASK,
        "targetTaskId": fresh,
        "newTarget": {
            "config": {"harness": "mock", "model": "fixture", "sandbox": "workspace-write", "permissionMode": "auto"},
            "title": "Fresh retry follow-up",
        },
    });
    let preview: Value = client
        .call_as(methods::PREVIEW_CONTEXT_TRANSFER, request.clone())
        .await
        .expect("preview the handoff");
    assert!(
        synced.workspace.chat(&fresh).unwrap().is_none(),
        "a preview creates nothing"
    );
    let applied = execute(
        &client,
        &fresh,
        "sw11-fresh-handoff",
        json!({"kind": "applyContextTransfer", "request": request, "previewId": preview["previewId"]}),
    )
    .await;
    assert_eq!(applied["status"], "applied", "{applied}");

    let created = synced.workspace.chat(&fresh).unwrap().expect("fresh task");
    assert_eq!(
        created.harness_session_id, None,
        "no provider session resumes"
    );
    assert!(!created.archived);
    assert_eq!(
        created
            .config
            .as_ref()
            .map(|config| (config.harness, config.permission_mode)),
        Some((HarnessId::Mock, PermissionMode::Auto)),
        "the fresh task runs with the settings chosen for it"
    );
    assert_eq!(
        created.source_context, None,
        "a task that never ran claims no branch or pull request"
    );
    assert_eq!(created.device_id, synced.device_id);
    assert_eq!(created.space_id.as_deref(), Some(PAST_SPACE));
    assert_eq!(created.cwd, codex_before.cwd, "same plain (non-Git) folder");
    let identity = synced
        .task_identity
        .view(&fresh)
        .expect("fresh task identity");
    assert_eq!(
        identity.task.lineage.source_task_id.as_deref(),
        Some(CODEX_TASK)
    );
    assert!(identity.current_binding.is_none() && identity.bindings.is_empty());

    let source = synced.workspace.chat(CODEX_TASK).unwrap().unwrap();
    assert!(source.archived, "handoff leaves the past task as it was");
    assert_eq!(
        source.harness_session_id.as_deref(),
        Some("codex-native-session")
    );
    assert_eq!(source.source_context, codex_before.source_context);
    synced.shutdown().await;
}
