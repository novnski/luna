//! E24 at the engine, through the production path: a REAL `ClaudeHarness`
//! drives the deterministic fake CLI (`crates/harness/tests/fixtures/
//! fake-claude.sh`, scenario `e24-engine`, 2.1.286 frame shapes from the
//! CLI's embedded SDK schema).
//!
//! A resumed Claude session reports a leftover background task — a turn
//! Claude starts by itself (`origin: task-notification`, no uuid echo) —
//! before it runs the human prompt. That foreign result must not complete the
//! human turn's receipt or transcript, must not flip the chat Idle, and must
//! not release a held queue row. Only the result echoing the human prompt's
//! native uuid settles it. Failure cases: `$B/evidence/cc02-events/
//! failure-cases.md` (B1, C6).

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry, RunJournal};
use luna_harness::ClaudeHarness;
use luna_proto::{AgentEvent, HarnessId, SessionStatus};

const CHAT: &str = "chat-e24";

fn fake_cli() -> PathBuf {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
}

fn text_of(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn queue_texts(core: &EngineCore) -> Vec<String> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|handle| handle.doc().read_queue().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|item| item.text)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e24_foreign_result_never_completes_the_human_receipt_or_releases_the_queue() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let accounts = AgentAccountsConfig {
        data_dir: data.join("provider-state"),
        claude_config_dir: root.path().join("synthetic-claude"),
        claude_config_file: root.path().join("synthetic-claude.json"),
        codex_home: root.path().join("synthetic-codex"),
    };
    std::fs::create_dir_all(&accounts.claude_config_dir).unwrap();
    std::fs::write(
        &accounts.claude_config_file,
        serde_json::json!({
            "oauthAccount": {"accountUuid": "claude-fixture", "emailAddress": "claude@example.test"}
        })
        .to_string(),
    )
    .unwrap();
    accounts.disable_claude_keychain_fallback().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake_cli())));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "dev-org", "dev-user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .expect("engine core assembles");
    core.workspace
        .create_space(
            "space-e24",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            luna_rpc::methods::MUTATE,
            serde_json::json!({"op": "createChat", "chatId": CHAT, "spaceId": "space-e24"}),
        )
        .await
        .expect("createChat");
    core.workspace.rename_chat(CHAT, "Pre-titled").unwrap();
    let status = || core.sessions.session_status(CHAT);

    // The human prompt dispatches a cold Claude run.
    let opening_id = core
        .doc_host
        .queue_message(CHAT, "opening scenario:e24-engine", Vec::new())
        .expect("queue opening");
    wait_for(
        || {
            entries(&core).iter().any(|e| {
                e.role == MessageRole::Assistant && text_of(e).contains("FOREIGN-LEFTOVER")
            })
        },
        "the leftover foreign turn to stream",
    )
    .await;
    // A second message typed while busy is held behind the turn.
    let second_id = core
        .doc_host
        .queue_message(CHAT, "second", Vec::new())
        .expect("queue second");

    // The foreign result has arrived (journaled evidence) yet completed nothing.
    let journal = RunJournal::open(data.join("orgs/dev-org/dev-user/journals")).unwrap();
    let foreign_record = || {
        journal
            .replay(CHAT, 0)
            .unwrap()
            .into_iter()
            .find_map(|(_, event)| match event {
                AgentEvent::ProviderEvidence {
                    kind,
                    native_id,
                    payload,
                    ..
                } if kind == "result" && native_id.as_deref() == Some("res-foreign") => {
                    Some(payload)
                }
                _ => None,
            })
    };
    wait_for(|| foreign_record().is_some(), "the foreign result evidence").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let record = foreign_record().unwrap();
    assert_eq!(record["owner"], "foreign");
    assert_eq!(record["withheld"], true);
    assert_eq!(queue_texts(&core), vec!["second"], "queue row stays held");
    let session = status().expect("session status");
    assert_eq!(session.status, SessionStatus::Working, "no premature Idle");
    assert_eq!(session.last_completed_turn, None, "no completion receipt");
    assert!(
        !entries(&core)
            .iter()
            .any(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)),
        "the foreign output must not complete the human turn: {:#?}",
        entries(&core)
    );
    assert!(
        !journal
            .replay(CHAT, 0)
            .unwrap()
            .iter()
            .any(|(_, event)| matches!(event, AgentEvent::Done { .. })),
        "no Done journaled for the foreign result"
    );

    // Release the human turn; the held row then dispatches.
    std::fs::write(project.join("e24-release"), b"go").unwrap();
    wait_for(
        || {
            entries(&core).iter().any(|e| {
                text_of(e).contains("SECOND-ANSWER") && e.status == Some(MessageStatus::Complete)
            })
        },
        "the queued follow-up to be answered",
    )
    .await;
    wait_for(|| queue_texts(&core).is_empty(), "the queue to drain").await;

    let all = entries(&core);
    let shape: Vec<(MessageRole, String, Option<MessageStatus>)> =
        all.iter().map(|e| (e.role, text_of(e), e.status)).collect();
    let position = |needle: &str| {
        all.iter()
            .position(|e| text_of(e).contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing: {shape:#?}"))
    };
    let opening = all
        .iter()
        .position(|e| e.id == opening_id)
        .expect("opening row");
    let foreign = position("FOREIGN-LEFTOVER");
    let human = position("HUMAN-ANSWER");
    let second = all
        .iter()
        .position(|e| e.id == second_id)
        .expect("second row");
    let second_answer = position("SECOND-ANSWER");
    assert!(
        opening < foreign && foreign < human && human < second && second < second_answer,
        "{shape:#?}"
    );
    assert_ne!(
        foreign, human,
        "foreign output keeps its own entry: {shape:#?}"
    );
    assert!(
        !text_of(&all[human]).contains("FOREIGN-LEFTOVER"),
        "{shape:#?}"
    );
    for index in [foreign, human, second_answer] {
        assert_eq!(
            all[index].status,
            Some(MessageStatus::Complete),
            "{shape:#?}"
        );
    }
    wait_for(
        || status().is_some_and(|s| s.status == SessionStatus::Idle),
        "Idle after the follow-up",
    )
    .await;
    assert_eq!(
        status().unwrap().last_completed_turn.as_deref(),
        Some(all[second_answer].id.as_str())
    );

    // Journal: exactly two Dones (human + follow-up), the foreign record
    // before the first, and post-Done evidence never reads as a died run.
    wait_for(
        || {
            matches!(
                journal.last_event(CHAT).unwrap(),
                Some((_, AgentEvent::ProviderEvidence { .. }))
            )
        },
        "parked-session evidence journaled after Done",
    )
    .await;
    let replay = journal.replay(CHAT, 0).unwrap();
    let dones: Vec<usize> = replay
        .iter()
        .enumerate()
        .filter_map(|(i, (_, e))| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(dones.len(), 2, "{replay:#?}");
    let foreign_at = replay
        .iter()
        .position(|(_, e)| matches!(e, AgentEvent::ProviderEvidence { native_id: Some(id), .. } if id == "res-foreign"))
        .unwrap();
    assert!(foreign_at < dones[0]);
    assert!(
        !journal
            .stale_sessions()
            .unwrap()
            .contains(&CHAT.to_string()),
        "trailing evidence is not run activity"
    );

    core.shutdown().await;
}
