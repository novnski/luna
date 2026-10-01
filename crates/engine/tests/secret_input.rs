//! Secret answers stay in memory. Failure cases:
//! `evidence/codex-adapter/failure-cases.md` §12, written before this test.
//!
//! Real Codex adapter against the deterministic peer
//! (`harness/tests/fixtures/codex-adapter-peer.py`), whose
//! `item/tool/requestUserInput` question carries `isSecret:true`. The whole
//! process logs at TRACE into a file so log output is scanned too.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, SessionCommandPayload, SessionCommandStatus};
use luna_engine::{
    AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry, SecretInputOutcome,
};
use luna_harness::CodexHarness;
use luna_proto::{AgentEvent, HarnessId, RunRequest, SandboxLevel, UserInputAnswer};

const SENTINEL: &str = "LUNA-SECRET-SENTINEL-9f2c41";

/// Process-wide TRACE log (the app's durable log is `{data}/logs/*.log`).
fn trace_log() -> &'static Path {
    static LOG: OnceLock<PathBuf> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::temp_dir().join(format!(
            "luna-secret-input-trace-{}.log",
            std::process::id()
        ));
        let file = std::fs::File::create(&path).unwrap();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(Arc::new(file))
                .finish(),
        )
        .unwrap();
        path
    })
}

fn engine(root: &Path) -> EngineCore {
    let peer = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/codex-adapter-peer.py");
    let accounts = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CodexHarness::new().with_executable(peer)));
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "synthetic-org", "synthetic-user"),
        Arc::new(registry),
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap()
}

fn run(core: &EngineCore, chat: &str, scenario: &str) {
    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: scenario.into(),
        harness: Some(HarnessId::Codex),
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
    };
    core.doc_host
        .queue_command_with_id_and_transfers(
            chat,
            &format!("{chat}-run"),
            SessionCommandPayload::Run {
                request,
                message_id: format!("{chat}-user"),
            },
            Vec::new(),
        )
        .unwrap();
}

async fn wait_for(mut done: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn entries_json(core: &EngineCore, chat: &str) -> String {
    let handle = core.doc_host.open(chat).unwrap();
    serde_json::to_string(&handle.doc().read_entries().unwrap()).unwrap()
}

/// The open (unresolved) secret request in a chat's transcript.
fn open_request(core: &EngineCore, chat: &str) -> Option<(String, String)> {
    let handle = core.doc_host.open(chat).ok()?;
    handle
        .doc()
        .read_entries()
        .ok()?
        .into_iter()
        .flat_map(|entry| entry.parts)
        .find_map(|part| match part {
            MessagePart::Input {
                request_id,
                questions,
                resolved: false,
                ..
            } if questions.iter().any(|q| q.is_secret()) => {
                Some((request_id, questions[0].id.clone()))
            }
            _ => None,
        })
}

fn answer(question_id: &str, value: &str) -> Vec<UserInputAnswer> {
    vec![UserInputAnswer {
        question_id: question_id.into(),
        labels: vec![value.into()],
    }]
}

/// Every file under `root` (SQLite + WAL, journals, docs, receipts, logs)
/// whose bytes contain `needle`.
fn files_containing(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path).is_ok_and(|bytes| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            }) {
                hits.push(path);
            }
        }
    }
    hits
}

fn assert_no_secret_anywhere(root: &Path) {
    assert_eq!(
        files_containing(root, SENTINEL),
        Vec::<PathBuf>::new(),
        "secret persisted under the engine data dir"
    );
    let log = std::fs::read_to_string(trace_log()).unwrap();
    assert!(log.len() > 1_000, "TRACE capture was active");
    assert!(!log.contains(SENTINEL), "secret reached the log");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_answer_is_delivered_in_memory_and_never_stored() {
    trace_log();
    let root = tempfile::tempdir().unwrap();
    let chat = "secret-live";
    let core = engine(root.path());
    let (_, mut events) = core.sessions.subscribe(chat, 0).unwrap();
    run(&core, chat, "secret-input");
    let (request_id, question_id) = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let AgentEvent::InputRequested {
                request_id,
                questions,
            } = events.recv().await.unwrap().event
            {
                assert!(questions[0].is_secret());
                break (request_id, questions[0].id.clone());
            }
        }
    })
    .await
    .expect("secret question parked");

    // The durable/generic path refuses the value outright.
    assert!(
        core.sessions
            .respond_input(chat, &request_id, answer(&question_id, SENTINEL))
            .is_err()
    );
    // The in-memory path hands it to the live resolver for this request.
    assert_eq!(
        core.sessions
            .respond_secret_input(chat, &request_id, answer(&question_id, SENTINEL)),
        SecretInputOutcome::Delivered
    );
    wait_for(
        || {
            let entries = entries_json(&core, chat);
            entries.contains("peer-verified") && entries.contains("\"resolved\":true")
        },
        "peer received the exact answer and the card resolved",
    )
    .await;
    let handle = core.doc_host.open(chat).unwrap();
    let commands = serde_json::to_string(&handle.doc().read_commands().unwrap()).unwrap();
    assert!(!commands.contains(SENTINEL), "secret in the command ledger");
    assert!(
        !entries_json(&core, chat).contains(SENTINEL),
        "secret in the transcript"
    );
    drop(handle);
    core.shutdown().await;
    drop(core);
    assert_no_secret_anywhere(root.path());
}

/// What a crash leaves behind: an unresolved secret question in the
/// transcript with no live resolver (no graceful shutdown ran).
fn crash_left_secret_card(core: &EngineCore, chat: &str) -> (String, String) {
    let handle = core.doc_host.open(chat).unwrap();
    let request = format!("{chat}-request");
    let question = luna_proto::UserInputQuestion {
        id: format!("{chat}-question"),
        header: "API token".into(),
        question: "Paste the deploy token".into(),
        options: Vec::new(),
        multi_select: false,
        approval: None,
        mcp: None,
        input: Some(luna_proto::UserInputDetails {
            option_descriptions: Vec::new(),
            allow_other: true,
            secret: true,
        }),
    };
    let writer = luna_doc::SegmentWriter::begin(
        handle.doc(),
        &format!("{chat}-assistant"),
        core.doc_host.device_id(),
        1,
    )
    .unwrap();
    writer
        .finish(
            &[MessagePart::Input {
                id: format!("{chat}-part"),
                request_id: request.clone(),
                questions: vec![question.clone()],
                resolved: false,
            }],
            luna_doc::MessageStatus::Aborted,
        )
        .unwrap();
    (request, question.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_request_after_restart_closes_without_replay() {
    trace_log();
    let root = tempfile::tempdir().unwrap();
    let parked = "secret-restart-parked";
    let core = engine(root.path());
    run(&core, parked, "secret-input-hold");
    wait_for(
        || open_request(&core, parked).is_some(),
        "secret question parked",
    )
    .await;
    // LUNA stops before the user answers.
    core.shutdown().await;
    drop(core);

    let core = engine(root.path());
    assert!(
        open_request(&core, parked).is_none(),
        "graceful shutdown ended the request (declined, nothing to replay)"
    );

    // A crash-left card: the in-memory path closes it and drops the answer.
    let crashed = "secret-restart-crashed";
    let (request_id, question_id) = crash_left_secret_card(&core, crashed);
    assert_eq!(
        core.sessions
            .respond_secret_input(crashed, &request_id, answer(&question_id, SENTINEL)),
        SecretInputOutcome::Closed,
        "ends cancelled, the answer is dropped"
    );
    assert!(open_request(&core, crashed).is_none(), "card closed");

    // An older client's durable answer to a crash-left card is refused,
    // closes the card and is never resumed as a new turn.
    let durable = "secret-restart-durable";
    let (request_id, question_id) = crash_left_secret_card(&core, durable);
    core.doc_host
        .queue_command_with_id_and_transfers(
            durable,
            "legacy-secret-answer",
            SessionCommandPayload::RespondInput {
                request_id,
                answers: answer(&question_id, "legacy-client-value"),
            },
            Vec::new(),
        )
        .unwrap();
    let handle = core.doc_host.open(durable).unwrap();
    wait_for(
        || {
            handle.doc().read_commands().unwrap().iter().any(|command| {
                command.id == "legacy-secret-answer"
                    && command.status != SessionCommandStatus::Pending
            })
        },
        "legacy durable answer evaluated",
    )
    .await;
    let command = handle
        .doc()
        .read_commands()
        .unwrap()
        .into_iter()
        .find(|command| command.id == "legacy-secret-answer")
        .unwrap();
    assert_eq!(
        command.status,
        SessionCommandStatus::Rejected,
        "{command:?}"
    );
    assert!(open_request(&core, durable).is_none(), "card closed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    for chat in [parked, crashed, durable] {
        let entries = handle_entries(&core, chat);
        assert!(
            !entries.iter().any(|entry| entry.role == MessageRole::User
                && serde_json::to_string(&entry.parts)
                    .unwrap()
                    .contains("Answering your earlier question")),
            "{chat}: the answer was replayed as a new turn"
        );
    }
    drop(handle);
    core.shutdown().await;
    drop(core);
    assert_no_secret_anywhere(root.path());
}

fn handle_entries(core: &EngineCore, chat: &str) -> Vec<luna_doc::SessionMessageEntry> {
    core.doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
}
