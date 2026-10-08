//! M2 end-to-end tests: doc-queued commands → host executor → harness stream →
//! journal + broadcast + folded doc entries, plus interrupt/recovery/idempotence
//! and the RPC surface over the in-memory transport.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SegmentWriter, SessionCommandEntry,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry,
};
use luna_engine::{EngineCore, HarnessRegistry, RunJournal};
use luna_harness::mock::MockHarness;
use luna_harness::{Harness, HarnessError, RunControls, SteerAcceptance};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode, ToolCall,
};
use luna_sync::DocsStore;

const CHAT: &str = "chat-e2e";

fn run_request(prompt: &str) -> RunRequest {
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
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn done(status: DoneStatus) -> AgentEvent {
    AgentEvent::Done {
        status,
        result: None,
        error: None,
        session_id: Some("hs-1".into()),
    }
}

fn mock_script() -> Vec<AgentEvent> {
    vec![
        AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: "/tmp".into(),
            session_id: "hs-1".into(),
            assistant_message_id: "a-1".into(),
        },
        AgentEvent::TextDelta { text: "Hel".into() },
        AgentEvent::TextDelta { text: "lo".into() },
        AgentEvent::ToolCall {
            id: "tool-1".into(),
            call: ToolCall::WriteFile {
                path: "/tmp/x".into(),
                content: Some("SECRET".into()),
            },
        },
        AgentEvent::ToolResult {
            id: "tool-1".into(),
            is_error: false,
            output: None,
            diff: None,
        },
        done(DoneStatus::Completed),
    ]
}

/// Scripted harness with a per-event delay; optionally hangs after the script until its
/// interrupt token cancels, then ends with `Done{interrupted}`.
struct ScriptedHarness {
    script: Vec<AgentEvent>,
    step_delay: Duration,
    hang_until_interrupt: bool,
}

#[async_trait]
impl Harness for ScriptedHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Scripted"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
        let script = self.script.clone();
        let delay = self.step_delay;
        let hang = self.hang_until_interrupt;
        let token = controls.interrupt.clone();
        tokio::spawn(async move {
            for event in script {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
                tokio::time::sleep(delay).await;
            }
            if hang {
                token.cancelled().await;
                let _ = tx.send(Ok(done(DoneStatus::Interrupted))).await;
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

/// Two-run harness for interrupt isolation. Run B stays parked until the test
/// releases it, then records whether its own token was cancelled before it
/// completes. This makes a cross-chat cancellation observable even if B has
/// not yet had a chance to publish `Done` when A's interrupt returns.
struct InterruptIsolationHarness {
    release_b: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Harness for InterruptIsolationHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Interrupt isolation"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(8);
        let release_b = self.release_b.clone();
        let token = controls.interrupt.clone();
        tokio::spawn(async move {
            if tx
                .send(Ok(AgentEvent::TextDelta {
                    text: format!("{} started", request.prompt),
                }))
                .await
                .is_err()
            {
                return;
            }

            if request.prompt == "run-b" {
                release_b.notified().await;
                if token.is_cancelled() {
                    let _ = tx.send(Ok(done(DoneStatus::Interrupted))).await;
                } else {
                    let _ = tx
                        .send(Ok(AgentEvent::TextDelta {
                            text: "; completed independently".into(),
                        }))
                        .await;
                    let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
                }
            } else {
                token.cancelled().await;
                let _ = tx.send(Ok(done(DoneStatus::Interrupted))).await;
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn registry_with(harness: Arc<dyn Harness>) -> Arc<HarnessRegistry> {
    let registry = HarnessRegistry::new();
    registry.register(harness);
    Arc::new(registry)
}

fn assemble(dir: &std::path::Path, harness: Arc<dyn Harness>) -> EngineCore {
    EngineCore::assemble(dir, registry_with(harness), HarnessId::Mock, None)
        .expect("engine core assembles")
}

/// Submit through the same owner admission path as the composer. Raw synced
/// rows intentionally cannot execute; remote authority has its own E2E suite.
fn queue_as_owner(core: &EngineCore, doc: &SessionDoc, id: &str, payload: SessionCommandPayload) {
    core.doc_host
        .queue_command_with_id_and_transfers(&doc.chat_id().unwrap(), id, payload, Vec::new())
        .expect("admit owner command");
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    entries_for(core, CHAT)
}

fn entries_for(core: &EngineCore, chat_id: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(chat_id)
        .expect("open chat")
        .doc()
        .read_entries()
        .expect("read entries")
}

/// Tolerant read for hot-polling predicates: a snapshot taken between a
/// segment writer's `push_container` and its field writes deserializes with
/// fields missing — treat that instant as "not yet" instead of panicking.
fn entries_now(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

fn command_status(core: &EngineCore, id: &str) -> Option<(SessionCommandStatus, Option<String>)> {
    core.doc_host
        .open(CHAT)
        .expect("open chat")
        .doc()
        .read_commands()
        .expect("read commands")
        .into_iter()
        .find(|c| c.id == id)
        .map(|c| (c.status, c.resolution))
}

#[tokio::test]
async fn queued_run_command_executes_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();

    // Live event subscription (journal replay + broadcast) before anything runs.
    let (replayed, mut live) = core.sessions.subscribe(CHAT, 0).unwrap();
    assert!(replayed.is_empty());

    // A viewer device queues the run command into the doc.
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-1",
        SessionCommandPayload::Run {
            request: run_request("do the thing"),
            message_id: "msg-user-1".into(),
        },
    );

    // The host executor picks it up, runs the harness, and the doc settles.
    wait_for(
        || {
            entries(&core).iter().any(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
        },
        "assistant entry to complete",
    )
    .await;

    let all = entries(&core);
    assert_eq!(all.len(), 2, "user + assistant entries, got {all:#?}");
    // User entry carries the command's client-minted message id.
    assert_eq!(all[0].id, "msg-user-1");
    assert_eq!(all[0].role, MessageRole::User);
    assert_eq!(
        all[0].parts,
        vec![MessagePart::Text {
            id: "t0".into(),
            text: "do the thing".into()
        }]
    );
    // Assistant entry: folded parts — merged text, then the resolved tool call with the
    // render-parts privacy policy applied (WriteFile content stripped).
    let assistant = &all[1];
    assert_eq!(assistant.status, Some(MessageStatus::Complete));
    assert_eq!(assistant.parts.len(), 2);
    match &assistant.parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "Hello"),
        other => panic!("unexpected first part {other:?}"),
    }
    match &assistant.parts[1] {
        MessagePart::Tool {
            call,
            resolved,
            is_error,
            ..
        } => {
            assert!(*resolved);
            assert!(!*is_error);
            assert_eq!(
                call,
                &ToolCall::WriteFile {
                    path: "/tmp/x".into(),
                    content: None
                }
            );
        }
        other => panic!("unexpected second part {other:?}"),
    }

    // Command outcome written by the host (sole outcome writer).
    assert_eq!(
        command_status(&core, "cmd-run-1"),
        Some((SessionCommandStatus::Applied, None))
    );

    // Journal replay: the full script in order, terminal Done last.
    let replay = core.sessions.subscribe(CHAT, 0).unwrap().0;
    assert_eq!(replay.len(), mock_script().len());
    assert!(matches!(
        replay.last().map(|j| &j.event),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
    let seqs: Vec<u64> = replay.iter().map(|j| j.seq).collect();
    assert_eq!(seqs, (1..=mock_script().len() as u64).collect::<Vec<_>>());

    // The live broadcast delivered the same events.
    let mut broadcast_count = 0usize;
    while let Ok(event) = live.try_recv() {
        assert!(event.seq >= 1);
        broadcast_count += 1;
    }
    assert_eq!(broadcast_count, mock_script().len());

    // Final session status: Idle.
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Idle)
    );
}

#[tokio::test]
async fn session_status_transitions_idle_working_idle() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(ScriptedHarness {
            script: mock_script(),
            step_delay: Duration::from_millis(40),
            hang_until_interrupt: false,
        }),
    );
    let mut watch = core.sessions.watch_sessions();
    assert!(watch.borrow().is_empty(), "no sessions before dispatch");

    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-status",
        SessionCommandPayload::Run {
            request: run_request("go"),
            message_id: "m-1".into(),
        },
    );

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status = tokio::time::timeout_at(deadline, watch.changed())
            .await
            .expect("status change before timeout")
            .map(|_| watch.borrow().first().map(|s| s.status))
            .expect("watch alive");
        if let Some(status) = status {
            if seen.last() != Some(&status) {
                seen.push(status);
            }
            if status == SessionStatus::Idle {
                break;
            }
        }
    }
    assert_eq!(seen, vec![SessionStatus::Working, SessionStatus::Idle]);
}

#[tokio::test]
async fn interrupt_stamps_streaming_entry_aborted() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(ScriptedHarness {
            script: vec![AgentEvent::TextDelta {
                text: "partial output".into(),
            }],
            step_delay: Duration::from_millis(5),
            hang_until_interrupt: true,
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-hang",
        SessionCommandPayload::Run {
            request: run_request("hang"),
            message_id: "m-1".into(),
        },
    );

    // Wait until the streaming entry is visibly in the doc, then interrupt via a
    // viewer-queued durable command (based_on = the streaming entry = current turn).
    wait_for(
        || {
            entries(&core)
                .iter()
                .any(|e| e.status == Some(MessageStatus::Streaming))
        },
        "streaming entry",
    )
    .await;
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-int-1",
        SessionCommandPayload::Interrupt {},
    );

    wait_for(
        || {
            entries(&core)
                .iter()
                .any(|e| e.status == Some(MessageStatus::Aborted))
                && command_status(&core, "cmd-int-1") == Some((SessionCommandStatus::Applied, None))
        },
        "aborted stamp",
    )
    .await;

    let all = entries(&core);
    let assistant = all
        .iter()
        .find(|e| e.role == MessageRole::Assistant)
        .unwrap();
    assert_eq!(assistant.status, Some(MessageStatus::Aborted));
    match &assistant.parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "partial output"),
        other => panic!("unexpected part {other:?}"),
    }
    assert_eq!(
        command_status(&core, "cmd-int-1"),
        Some((SessionCommandStatus::Applied, None))
    );
    // Journal closed with a Done — nothing left to recover.
    let journal = RunJournal::open(dir.path().join("orgs/dev-org/dev-user/journals")).unwrap();
    assert!(journal.stale_sessions().unwrap().is_empty());
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Idle)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_is_scoped_to_the_target_chat() {
    const CHAT_A: &str = "chat-interrupt-a";
    const CHAT_B: &str = "chat-interrupt-b";

    let dir = tempfile::tempdir().unwrap();
    let release_b = Arc::new(tokio::sync::Notify::new());
    let core = assemble(
        dir.path(),
        Arc::new(InterruptIsolationHarness {
            release_b: release_b.clone(),
        }),
    );

    for (chat_id, command_id, message_id, prompt) in [
        (CHAT_A, "cmd-run-a", "message-a", "run-a"),
        (CHAT_B, "cmd-run-b", "message-b", "run-b"),
    ] {
        let handle = core.doc_host.open(chat_id).unwrap();
        queue_as_owner(
            &core,
            handle.doc(),
            command_id,
            SessionCommandPayload::Run {
                request: run_request(prompt),
                message_id: message_id.into(),
            },
        );
    }

    wait_for(
        || {
            [CHAT_A, CHAT_B].into_iter().all(|chat_id| {
                core.sessions.session_status(chat_id).map(|s| s.status)
                    == Some(SessionStatus::Working)
                    && entries_for(&core, chat_id)
                        .iter()
                        .any(|entry| entry.status == Some(MessageStatus::Streaming))
            })
        },
        "both chats to be streaming",
    )
    .await;

    assert!(core.sessions.interrupt(CHAT_A).await.unwrap());
    assert_eq!(
        core.sessions.session_status(CHAT_A).map(|s| s.status),
        Some(SessionStatus::Idle)
    );
    assert!(
        entries_for(&core, CHAT_A)
            .iter()
            .any(|entry| entry.status == Some(MessageStatus::Aborted))
    );
    assert!(
        !core.sessions.interrupt(CHAT_A).await.unwrap(),
        "a settled chat must report that there is no live run to interrupt"
    );

    // B has not observed any completion signal yet. Release it through its
    // independent test control; it checks its own token before completing, so
    // a cancellation leaked from A cannot hide behind an early status read.
    assert_eq!(
        core.sessions.session_status(CHAT_B).map(|s| s.status),
        Some(SessionStatus::Working),
        "interrupting chat A must leave chat B running"
    );
    assert!(
        entries_for(&core, CHAT_B)
            .iter()
            .any(|entry| entry.status == Some(MessageStatus::Streaming))
    );
    release_b.notify_one();

    wait_for(
        || core.sessions.session_status(CHAT_B).map(|s| s.status) == Some(SessionStatus::Idle),
        "chat B to complete independently",
    )
    .await;

    let assistant_b = entries_for(&core, CHAT_B)
        .into_iter()
        .find(|entry| entry.role == MessageRole::Assistant)
        .expect("chat B assistant entry");
    assert_eq!(assistant_b.status, Some(MessageStatus::Complete));
    assert!(assistant_b.parts.iter().any(|part| {
        matches!(part, MessagePart::Text { text, .. } if text.contains("completed independently"))
    }));
}

#[tokio::test]
async fn steer_with_no_live_run_falls_back_to_new_turn() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();

    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-1",
        SessionCommandPayload::Run {
            request: run_request("first"),
            message_id: "m-1".into(),
        },
    );
    wait_for(
        || {
            matches!(
                command_status(&core, "cmd-run-1"),
                Some((SessionCommandStatus::Applied, _))
            )
        },
        "first run applied",
    )
    .await;
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "first run settled",
    )
    .await;

    // No live run anymore (mock finishes instantly): a steer command must fall back to
    // dispatch-as-next-turn, per luna's executor.
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-steer-1",
        SessionCommandPayload::Steer {
            attachments: Vec::new(),
            input_items: Vec::new(),
            prompt: "also do this".into(),
            message_id: Some("m-2".into()),
        },
    );
    wait_for(
        || {
            matches!(
                command_status(&core, "cmd-steer-1"),
                Some((SessionCommandStatus::Applied, Some(_)))
            )
        },
        "steer fallback applied",
    )
    .await;
    let (status, resolution) = command_status(&core, "cmd-steer-1").unwrap();
    assert_eq!(status, SessionCommandStatus::Applied);
    assert_eq!(resolution.as_deref(), Some("queued as new turn"));

    wait_for(
        || {
            entries(&core)
                .iter()
                .filter(|e| {
                    e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
                })
                .count()
                == 2
        },
        "second assistant entry",
    )
    .await;
    // The steer prompt became a user entry with its client-minted id.
    assert!(
        entries(&core)
            .iter()
            .any(|e| e.id == "m-2" && e.role == MessageRole::User)
    );
}

#[tokio::test]
async fn processed_commands_are_skipped_on_redelivery() {
    let dir = tempfile::tempdir().unwrap();

    // Simulate a crash AFTER mark-processed but BEFORE execute/outcome: the ledger has
    // the id, the doc still says pending.
    {
        let store = DocsStore::open(dir.path().join("orgs/dev-org/dev-user")).unwrap();
        assert!(store.mark_processed("cmd-crashed").unwrap());
    }

    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-crashed",
        SessionCommandPayload::Run {
            request: run_request("never again"),
            message_id: "m-x".into(),
        },
    );

    // Give the drain a moment: the command must not EXECUTE — no user entry,
    // no run. But it must not stay a forever-Pending ghost either (v0.2.12
    // swallowed-send: "Sending…" forever, retry a no-op): the dead-command
    // sweep terminalizes it as Rejected so the doc tells the truth and a
    // retry can mint a fresh attempt.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        entries(&core).is_empty(),
        "skipped command must not execute"
    );
    wait_for(
        || {
            matches!(
                command_status(&core, "cmd-crashed"),
                Some((SessionCommandStatus::Rejected, _))
            )
        },
        "crash-window command terminalized as Rejected",
    )
    .await;
    assert!(core.sessions.session_status(CHAT).is_none());

    // Direct ledger-evaluation check: re-evaluating a processed command = Skip.
    let store = DocsStore::open(dir.path().join("orgs/dev-org/dev-user")).unwrap();
    let commands = handle.doc().read_commands().unwrap();
    let entry = commands.iter().find(|c| c.id == "cmd-crashed").unwrap();
    let is_processed = |id: &str| store.is_processed(id).unwrap_or(false);
    let never_past = |_: &str| false;
    let verdict = luna_doc::evaluate_command(
        entry,
        &luna_doc::EvaluationContext {
            is_processed: &is_processed,
            now_ms: chrono::Utc::now().timestamp_millis(),
            entries: &commands,
            current_turn_id: None,
            turn_is_past: &never_past,
        },
    );
    assert_eq!(verdict, luna_doc::CommandDisposition::Skip);
}

/// The v0.2.12 field report: a send whose command was consumed by the ledger
/// but never executed (crash between mark and resolve) was invisible to
/// every retry — the drain filters processed ids, so the session was dead
/// forever while new sessions worked. Retry must mint a FRESH attempt.
#[tokio::test]
async fn retry_reissues_a_swallowed_send() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = DocsStore::open(dir.path().join("orgs/dev-org/dev-user")).unwrap();
        assert!(store.mark_processed("cmd-dead").unwrap());
    }
    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-dead",
        SessionCommandPayload::Run {
            request: run_request("try again"),
            message_id: "m-retry".into(),
        },
    );
    // The sweep terminalizes the dead attempt without executing it…
    wait_for(
        || {
            matches!(
                command_status(&core, "cmd-dead"),
                Some((SessionCommandStatus::Rejected, _))
            )
        },
        "dead attempt rejected",
    )
    .await;
    assert!(entries(&core).is_empty(), "dead attempt must not execute");

    // …and the user's retry mints a fresh attempt that actually runs.
    core.doc_host.retry_delivery(CHAT).unwrap();
    wait_for(
        || {
            entries_now(&core)
                .iter()
                .any(|e| e.id == "m-retry" && e.role == MessageRole::User)
        },
        "re-issued send writes the user entry",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
        },
        "re-issued send runs to completion",
    )
    .await;
    let run_attempts = |cmds: &[SessionCommandEntry]| {
        cmds.iter()
            .filter(|c| {
                matches!(&c.payload,
                    SessionCommandPayload::Run { message_id, .. } if message_id == "m-retry")
            })
            .count()
    };
    assert_eq!(
        run_attempts(&handle.doc().read_commands().unwrap()),
        2,
        "original + exactly one re-issue"
    );
    // A delivered message must never re-issue: retry while healthy is a no-op.
    core.doc_host.retry_delivery(CHAT).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        run_attempts(&handle.doc().read_commands().unwrap()),
        2,
        "retry after delivery must not duplicate the send"
    );
}

#[tokio::test]
async fn recover_stale_journal_stamps_aborted_on_boot() {
    let dir = tempfile::tempdir().unwrap();
    let device_id = "dev-host-fixed";
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(dir.path().join("device-id"), device_id).unwrap();

    // Craft the crash state: a journal without a terminal Done + a doc snapshot whose
    // assistant entry is still `streaming`.
    {
        let journal = RunJournal::open(dir.path().join("orgs/dev-org/dev-user/journals")).unwrap();
        journal
            .append(
                CHAT,
                &AgentEvent::TextDelta {
                    text: "doomed".into(),
                },
            )
            .unwrap();

        let doc = SessionDoc::init(CHAT).unwrap();
        doc.push_message(&SessionMessageEntry {
            id: "m-user".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: "hi".into(),
            }],
            created_at: 1,
            device_id: device_id.into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
        let mut writer = SegmentWriter::begin(&doc, "m-assist", device_id, 2).unwrap();
        writer
            .sync(&[MessagePart::Text {
                id: "t0".into(),
                text: "doomed".into(),
            }])
            .unwrap();
        // No finish — the "process" dies here with the entry still streaming.
        let store = DocsStore::open(dir.path().join("orgs/dev-org/dev-user")).unwrap();
        store
            .save_snapshot(CHAT, &doc.export_snapshot().unwrap())
            .unwrap();
    }

    // Boot: EngineCore::assemble runs recover_stale.
    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    assert_eq!(core.device_id, device_id);

    let all = entries(&core);
    let assistant = all.iter().find(|e| e.id == "m-assist").unwrap();
    assert_eq!(assistant.status, Some(MessageStatus::Aborted));
    match &assistant.parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "doomed"),
        other => panic!("unexpected part {other:?}"),
    }

    // Journal closed with a synthetic Done{interrupted}; no longer stale.
    let journal = RunJournal::open(dir.path().join("orgs/dev-org/dev-user/journals")).unwrap();
    assert!(journal.stale_sessions().unwrap().is_empty());
    let (_, last) = journal.last_event(CHAT).unwrap().unwrap();
    assert!(matches!(
        last,
        AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        }
    ));
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Idle)
    );
}

#[tokio::test]
async fn rpc_surface_over_in_memory_transport() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    let client = luna_rpc::memory_client(core.rpc_service());

    // ListHarnesses + ListModels.
    let harnesses = client
        .call(luna_rpc::methods::LIST_HARNESSES, serde_json::Value::Null)
        .await
        .unwrap();
    assert_eq!(harnesses[0]["id"], "mock");
    let models = client
        .call(
            luna_rpc::methods::LIST_MODELS,
            serde_json::json!({"harness": "mock"}),
        )
        .await
        .unwrap();
    assert_eq!(models[0]["id"], "mock-1");

    // WatchSessions + WatchDocMessages streams.
    let mut sessions_stream = client
        .subscribe(luna_rpc::methods::WATCH_SESSIONS, serde_json::Value::Null)
        .await
        .unwrap();
    let first_sessions = tokio::time::timeout(Duration::from_secs(5), sessions_stream.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_sessions, serde_json::json!([]));

    let mut messages_stream = client
        .subscribe(
            luna_rpc::methods::WATCH_DOC_MESSAGES,
            serde_json::json!({"chatId": CHAT}),
        )
        .await
        .unwrap();
    let initial = tokio::time::timeout(Duration::from_secs(5), messages_stream.recv())
        .await
        .unwrap()
        .unwrap();
    // Delta protocol: the stream opens with a full reset frame, including
    // the (empty) set of entries a fork/edit/rewind boundary can resolve.
    assert_eq!(
        initial,
        serde_json::json!({ "reset": [], "contextUsage": null,
            "contextControl": { "schemaVersion": 1, "compact": { "status": "idle" } },
            "replayBaseline": { "entries": {} }, "checkpointed": [] })
    );

    // QueueCommand (as this device's composer would over IPC).
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: run_request("via rpc"),
        message_id: "m-rpc-1".into(),
    })
    .unwrap();
    let queued = client
        .call(
            luna_rpc::methods::QUEUE_COMMAND,
            serde_json::json!({"chatId": CHAT, "command": command}),
        )
        .await
        .unwrap();
    assert!(queued["commandId"].is_string());

    // The doc-messages stream emits delta frames until the transcript settles:
    // user entry + completed assistant entry with the folded parts. Applying
    // each frame client-side mirrors what both viewports do.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut materialized: Vec<SessionMessageEntry> = vec![];
    let settled = loop {
        let item = tokio::time::timeout_at(deadline, messages_stream.recv())
            .await
            .expect("doc messages before timeout")
            .expect("stream alive");
        let frame: luna_doc::TranscriptFrame = serde_json::from_value(item).unwrap();
        luna_doc::apply_transcript_frame(&mut materialized, frame).unwrap();
        if materialized.len() == 2 && materialized[1].status == Some(MessageStatus::Complete) {
            break materialized;
        }
    };
    assert_eq!(settled[0].id, "m-rpc-1");
    assert_eq!(settled[0].role, MessageRole::User);
    match &settled[1].parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "Hello"),
        other => panic!("unexpected part {other:?}"),
    }

    // WatchSessions eventually reports the settled Idle session.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let item = tokio::time::timeout_at(deadline, sessions_stream.recv())
            .await
            .expect("session update before timeout")
            .expect("stream alive");
        let list: Vec<serde_json::Value> = serde_json::from_value(item).unwrap();
        if list.first().and_then(|s| s["status"].as_str()) == Some("idle") {
            break;
        }
    }
}

/// Exercises the real engine RPC and device-root persistence with a provider
/// whose catalog can change between calls and fail after a good result.
#[tokio::test]
async fn model_catalog_cache_survives_refresh_failure_and_engine_reopen() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CatalogHarness {
        answer: Arc<Mutex<Result<Vec<Model>, String>>>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Harness for CatalogHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Catalog fixture"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer
                .lock()
                .unwrap()
                .clone()
                .map_err(HarnessError::Protocol)
        }
        async fn run(
            &self,
            _request: RunRequest,
            _controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            Err(HarnessError::Protocol(
                "catalog fixture does not run turns".into(),
            ))
        }
    }

    let model = |id: &str| Model {
        id: id.into(),
        label: id.into(),
        description: None,
        reasoning_levels: Vec::new(),
        options: Vec::new(),
        first_seen_at_ms: None,
    };
    let root = tempfile::tempdir().unwrap();
    let answer = Arc::new(Mutex::new(Ok(vec![model("first")])));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = || -> Arc<dyn Harness> {
        Arc::new(CatalogHarness {
            answer: answer.clone(),
            calls: calls.clone(),
        })
    };
    let core = assemble(root.path(), provider());
    let client = luna_rpc::memory_client(core.rpc_service());
    let list = |refresh: bool| serde_json::json!({"harness": "mock", "refresh": refresh});

    let first = client
        .call(luna_rpc::methods::LIST_MODELS, list(false))
        .await
        .unwrap();
    assert_eq!(first[0]["id"], "first");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_dir(root.path().join("model-cache"))
            .unwrap()
            .count(),
        1
    );

    *answer.lock().unwrap() = Ok(vec![model("second")]);
    let cached = client
        .call(luna_rpc::methods::LIST_MODELS, list(false))
        .await
        .unwrap();
    assert_eq!(cached[0]["id"], "first");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let refreshed = client
        .call(luna_rpc::methods::LIST_MODELS, list(true))
        .await
        .unwrap();
    assert_eq!(refreshed[0]["id"], "second");
    *answer.lock().unwrap() = Err("offline".into());
    assert!(
        client
            .call(luna_rpc::methods::LIST_MODELS, list(true))
            .await
            .is_err()
    );
    assert_eq!(
        client
            .call(luna_rpc::methods::LIST_MODELS, list(false))
            .await
            .unwrap()[0]["id"],
        "second"
    );

    drop(client);
    drop(core);
    let reopened = assemble(root.path(), provider());
    let reopened_client = luna_rpc::memory_client(reopened.rpc_service());
    assert_eq!(
        reopened_client
            .call(luna_rpc::methods::LIST_MODELS, list(false))
            .await
            .unwrap()[0]["id"],
        "second"
    );
}

#[tokio::test]
async fn respond_input_resolves_pending_question() {
    // Harness that asks a question through RunControls and echoes the answer.
    struct AskingHarness;
    #[async_trait]
    impl Harness for AskingHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Asking"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
            tokio::spawn(async move {
                let answers = (controls.request_input)(vec![luna_proto::UserInputQuestion {
                    id: "q1".into(),
                    header: "Pick".into(),
                    question: "Which one?".into(),
                    options: vec!["a".into(), "b".into()],
                    multi_select: false,
                    mcp: None,
                    approval: None,
                    input: None,
                }])
                .await
                .unwrap_or_default();
                let picked = answers
                    .first()
                    .and_then(|a| a.labels.first().cloned())
                    .unwrap_or_else(|| "none".into());
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: format!("picked {picked}"),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(AskingHarness));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-ask",
        SessionCommandPayload::Run {
            request: run_request("ask me"),
            message_id: "m-1".into(),
        },
    );

    // The input request surfaces: status AwaitingInput + an unresolved input part.
    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingInput)
        },
        "awaiting input",
    )
    .await;
    wait_for(
        || {
            entries(&core).iter().any(|e| {
                e.parts.iter().any(|p| {
                    matches!(
                        p,
                        MessagePart::Input {
                            resolved: false,
                            ..
                        }
                    )
                })
            })
        },
        "input part in doc",
    )
    .await;

    // A viewer answers through the durable command queue.
    let request_id = entries(&core)
        .iter()
        .find_map(|e| {
            e.parts.iter().find_map(|p| match p {
                MessagePart::Input { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
        })
        .unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-answer-1",
        SessionCommandPayload::RespondInput {
            request_id,
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "q1".into(),
                labels: vec!["b".into()],
            }],
        },
    );

    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.status == Some(MessageStatus::Complete)
                    && e.parts
                        .iter()
                        .any(|p| matches!(p, MessagePart::Text { text, .. } if text == "picked b"))
            })
        },
        "answered turn to complete",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-answer-1"),
        Some((SessionCommandStatus::Applied, None))
    );
    // The input part is marked resolved in the doc.
    assert!(entries(&core).iter().any(|e| {
        e.parts
            .iter()
            .any(|p| matches!(p, MessagePart::Input { resolved: true, .. }))
    }));
    // The run task writes the Complete entry BEFORE settling the status row —
    // wait for the transition instead of asserting the instant in between.
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "session to settle idle",
    )
    .await;
}

#[tokio::test]
async fn typed_live_approval_rejects_respond_input_and_accepts_only_respond_approval() {
    struct ApprovalHarness;
    #[async_trait]
    impl Harness for ApprovalHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Approval"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                let answers = (controls.request_input)(vec![luna_proto::UserInputQuestion {
                    id: "approval-question".into(),
                    header: "Approve command".into(),
                    question: "Allow echo safe?".into(),
                    options: vec!["Deny".into(), "Allow once".into()],
                    multi_select: false,
                    mcp: None,
                    approval: Some(luna_proto::ApprovalRequest {
                        provider_request_id: "provider-approval".into(),
                        provider: HarnessId::Mock,
                        turn_id: None,
                        machine_name: "test-machine".into(),
                        action_kind: luna_proto::ApprovalActionKind::Command,
                        summary: "echo safe".into(),
                        requested_scope: luna_proto::PermissionScope::Workspace,
                        expires_at_ms: i64::MAX,
                        supports_task_allowance: false,
                        details: None,
                    }),
                    input: None,
                }])
                .await
                .unwrap_or_default();
                let picked = answers
                    .first()
                    .and_then(|answer| answer.labels.first())
                    .cloned()
                    .unwrap_or_else(|| "none".into());
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: format!("approval {picked}"),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(ApprovalHarness));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-approval",
        SessionCommandPayload::Run {
            request: run_request("approve"),
            message_id: "m-approval".into(),
        },
    );
    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingApproval)
        },
        "awaiting typed approval",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.parts.iter().any(|part| {
                    matches!(
                        part,
                        MessagePart::Input {
                            resolved: false,
                            ..
                        }
                    )
                })
            })
        },
        "typed approval part appears",
    )
    .await;
    let request_id = entries(&core)
        .iter()
        .find_map(|entry| {
            entry.parts.iter().find_map(|part| match part {
                MessagePart::Input {
                    request_id,
                    resolved: false,
                    ..
                } => Some(request_id.clone()),
                _ => None,
            })
        })
        .unwrap();

    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-approval-bypass",
        SessionCommandPayload::RespondInput {
            request_id: request_id.clone(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "approval-question".into(),
                labels: vec!["Allow once".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "cmd-approval-bypass")
                .is_some_and(|(status, _)| status != SessionCommandStatus::Pending)
        },
        "ordinary input bypass rejected",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-approval-bypass"),
        Some((
            SessionCommandStatus::Rejected,
            Some("no pending input request".into())
        ))
    );
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::AwaitingApproval)
    );
    assert!(
        entries(&core)
            .iter()
            .any(|entry| entry.parts.iter().any(|part| matches!(
                part,
                MessagePart::Input {
                    resolved: false,
                    ..
                }
            )))
    );

    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-approval-right",
        SessionCommandPayload::RespondApproval {
            request_id,
            decision: luna_proto::ApprovalDecision::AllowOnce,
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.parts.iter().any(
                    |part| matches!(part, MessagePart::Text { text, .. } if text == "approval Allow once"),
                )
            })
        },
        "typed approval completes",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-approval-right"),
        Some((SessionCommandStatus::Applied, None))
    );
}

#[tokio::test]
async fn structured_approval_preserves_details_and_rejects_unadvertised_choice() {
    struct ApprovalHarness {
        prompts: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Harness for ApprovalHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Approval"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            self.prompts.lock().unwrap().push(request.prompt.clone());
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            // Like a real adapter, confirm the provider session first: that is
            // what settles a claimed queue row into the transcript.
            let started = AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "hs-1".into(),
                assistant_message_id: format!("a-{}", request.prompt),
            };
            tokio::spawn(async move {
                let _ = tx.send(Ok(started)).await;
                let answers = (controls.request_input)(vec![luna_proto::UserInputQuestion {
                    id: "approval-question".into(),
                    header: "Approve command".into(),
                    question: "Allow echo safe?".into(),
                    options: vec!["Deny".into(), "Allow once".into(), "Cancel turn".into()],
                    multi_select: false,
                    mcp: None,
                    input: None,
                    approval: Some(luna_proto::ApprovalRequest {
                        provider_request_id: "provider-approval".into(),
                        provider: HarnessId::Mock,
                        turn_id: None,
                        machine_name: "test-machine".into(),
                        action_kind: luna_proto::ApprovalActionKind::Command,
                        summary: "echo safe".into(),
                        requested_scope: luna_proto::PermissionScope::Workspace,
                        expires_at_ms: i64::MAX,
                        supports_task_allowance: false,
                        details: Some(luna_proto::ApprovalDetails {
                            cwd: Some("/tmp/permission-fixture".into()),
                            command: Some("echo safe".into()),
                            choices: vec![
                                luna_proto::ApprovalChoice {
                                    label: "Deny".into(),
                                    scope: "none".into(),
                                    description: "Reject".into(),
                                },
                                luna_proto::ApprovalChoice {
                                    label: "Allow once".into(),
                                    scope: "request".into(),
                                    description: "Only this request".into(),
                                },
                                luna_proto::ApprovalChoice {
                                    label: "Cancel turn".into(),
                                    scope: "turn".into(),
                                    description: "Interrupt this turn".into(),
                                },
                            ],
                            ..Default::default()
                        }),
                    }),
                }])
                .await
                .unwrap_or_default();
                let picked = answers
                    .first()
                    .and_then(|answer| answer.labels.first())
                    .cloned()
                    .unwrap_or_else(|| "none".into());
                if picked == "Cancel turn" {
                    // Mirrors the Codex adapter: native Cancel ends the turn
                    // with turn/completed status=interrupted and no local Stop.
                    let _ = tx.send(Ok(done(DoneStatus::Interrupted))).await;
                    return;
                }
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: format!("approval {picked}"),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let core = assemble(
        dir.path(),
        Arc::new(ApprovalHarness {
            prompts: prompts.clone(),
        }),
    );
    // A real chat row gives queued follow-ups the same local authority and
    // run configuration the composer queue uses.
    core.workspace
        .create_space("space-approval", &core.device_id, "/tmp", None, false)
        .expect("create project");
    luna_rpc::memory_client(core.rpc_service())
        .call(
            luna_rpc::methods::MUTATE,
            serde_json::json!({"op": "createChat", "chatId": CHAT, "spaceId": "space-approval"}),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-approval",
        SessionCommandPayload::Run {
            request: run_request("approve"),
            message_id: "m-approval".into(),
        },
    );
    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingApproval)
        },
        "awaiting typed approval",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.parts.iter().any(|part| {
                    matches!(
                        part,
                        MessagePart::Input {
                            resolved: false,
                            ..
                        }
                    )
                })
            })
        },
        "typed approval part appears",
    )
    .await;
    let request_id = entries(&core)
        .iter()
        .find_map(|entry| {
            entry.parts.iter().find_map(|part| match part {
                MessagePart::Input {
                    request_id,
                    resolved: false,
                    ..
                } => Some(request_id.clone()),
                _ => None,
            })
        })
        .unwrap();

    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-approval-bypass",
        SessionCommandPayload::RespondInput {
            request_id: request_id.clone(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "approval-question".into(),
                labels: vec!["Allow once".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "cmd-approval-bypass")
                .is_some_and(|(status, _)| status != SessionCommandStatus::Pending)
        },
        "ordinary input bypass rejected",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-approval-bypass"),
        Some((
            SessionCommandStatus::Rejected,
            Some("no pending input request".into())
        ))
    );
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::AwaitingApproval)
    );
    assert!(
        entries(&core)
            .iter()
            .any(|entry| entry.parts.iter().any(|part| matches!(
                part,
                MessagePart::Input {
                    resolved: false,
                    ..
                }
            )))
    );

    let stored = entries(&core)
        .into_iter()
        .flat_map(|entry| entry.parts)
        .find_map(|part| match part {
            MessagePart::Input {
                questions,
                resolved: false,
                ..
            } => questions.into_iter().find_map(|question| question.approval),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        stored.details.as_ref().unwrap().cwd.as_deref(),
        Some("/tmp/permission-fixture")
    );
    assert!(
        !core
            .sessions
            .respond_approval(
                CHAT,
                &request_id,
                luna_proto::ApprovalDecision::ProviderChoice { index: u32::MAX }
            )
            .unwrap()
    );
    // Queued behind the approval: a normal completion must still release it.
    core.doc_host
        .queue_message(CHAT, "after allow", Vec::new())
        .expect("queue behind approval");
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-approval-right",
        SessionCommandPayload::RespondApproval {
            request_id,
            decision: luna_proto::ApprovalDecision::ProviderChoice { index: 1 },
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.parts.iter().any(
                    |part| matches!(part, MessagePart::Text { text, .. } if text == "approval Allow once"),
                )
            })
        },
        "typed approval completes",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-approval-right"),
        Some((SessionCommandStatus::Applied, None))
    );

    // Normal completion drains the queue: the follow-up becomes the next turn.
    wait_for(
        || prompts.lock().unwrap().iter().any(|p| p == "after allow"),
        "queued follow-up drains after a normal completion",
    )
    .await;
    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingApproval)
        },
        "follow-up awaits approval",
    )
    .await;
    let mut cancel_request = None;
    wait_for(
        || {
            cancel_request = entries_now(&core).iter().find_map(|entry| {
                entry.parts.iter().find_map(|part| match part {
                    MessagePart::Input {
                        request_id,
                        resolved: false,
                        ..
                    } => Some(request_id.clone()),
                    _ => None,
                })
            });
            cancel_request.is_some()
        },
        "second typed approval part appears",
    )
    .await;
    core.doc_host
        .queue_message(CHAT, "held after cancel", Vec::new())
        .expect("queue behind the cancelled turn");

    // Native Cancel (no local Stop): the provider settles the turn as
    // interrupted. The queue must be held exactly as after Stop.
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-approval-cancel",
        SessionCommandPayload::RespondApproval {
            request_id: cancel_request.unwrap(),
            decision: luna_proto::ApprovalDecision::ProviderChoice { index: 2 },
        },
    );
    wait_for(
        || {
            !core.sessions.turn_in_flight(CHAT)
                && entries_now(&core).iter().any(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Aborted)
                })
        },
        "native Cancel settles the turn as aborted",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let queued = handle
        .doc()
        .read_queue()
        .unwrap()
        .into_iter()
        .map(|item| item.text)
        .collect::<Vec<_>>();
    assert_eq!(
        queued,
        vec!["held after cancel".to_string()],
        "native Cancel must hold the queued message like Stop"
    );
    assert_eq!(
        *prompts.lock().unwrap(),
        vec!["approve".to_string(), "after allow".to_string()],
        "native Cancel must neither replay the cancelled turn nor send the held row"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn recovered_typed_approval_cannot_replay_as_input_but_ordinary_question_can() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(MockHarness {
            script: mock_script(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-seed-run",
        SessionCommandPayload::Run {
            request: run_request("seed config"),
            message_id: "m-seed".into(),
        },
    );
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "seed run settles",
    )
    .await;

    handle
        .doc()
        .push_message(&SessionMessageEntry {
            id: "ordinary-input".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Input {
                id: "ordinary-part".into(),
                request_id: "ordinary-request".into(),
                questions: vec![luna_proto::UserInputQuestion {
                    id: "ordinary-question".into(),
                    header: "Pick".into(),
                    question: "Which one?".into(),
                    options: vec!["a".into(), "b".into()],
                    multi_select: false,
                    mcp: None,
                    approval: None,
                    input: None,
                }],
                resolved: false,
            }],
            created_at: 10,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-recovered-ordinary",
        SessionCommandPayload::RespondInput {
            request_id: "ordinary-request".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "ordinary-question".into(),
                labels: vec!["b".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "cmd-recovered-ordinary")
                .is_some_and(|(status, _)| status != SessionCommandStatus::Pending)
        },
        "ordinary recovered answer applies",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-recovered-ordinary"),
        Some((
            SessionCommandStatus::Applied,
            Some("answered as new turn".into())
        ))
    );
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "ordinary recovery run settles",
    )
    .await;

    handle
        .doc()
        .push_message(&SessionMessageEntry {
            id: "approval-input".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Input {
                id: "approval-part".into(),
                request_id: "approval-request".into(),
                questions: vec![luna_proto::UserInputQuestion {
                    id: "approval-question".into(),
                    header: "Approval".into(),
                    question: "Allow?".into(),
                    options: vec!["Deny".into(), "Allow once".into()],
                    multi_select: false,
                    mcp: None,
                    approval: Some(luna_proto::ApprovalRequest {
                        provider_request_id: "provider-request".into(),
                        provider: HarnessId::Mock,
                        turn_id: None,
                        machine_name: "test-machine".into(),
                        action_kind: luna_proto::ApprovalActionKind::Command,
                        summary: "echo safe".into(),
                        requested_scope: luna_proto::PermissionScope::Workspace,
                        expires_at_ms: i64::MAX,
                        supports_task_allowance: false,
                        details: None,
                    }),
                    input: None,
                }],
                resolved: false,
            }],
            created_at: 20,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-recovered-bypass",
        SessionCommandPayload::RespondInput {
            request_id: "approval-request".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "approval-question".into(),
                labels: vec!["Allow once".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "cmd-recovered-bypass")
                .is_some_and(|(status, _)| status != SessionCommandStatus::Pending)
        },
        "recovered approval bypass rejected",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-recovered-bypass"),
        Some((
            SessionCommandStatus::Rejected,
            Some("typed approval requires respondApproval".into())
        ))
    );
    assert!(handle.doc().read_entries().unwrap().iter().any(|entry| {
        entry.parts.iter().any(|part| {
            matches!(
                part,
                MessagePart::Input {
                    request_id,
                    resolved: false,
                    ..
                } if request_id == "approval-request"
            )
        })
    }));

    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-recovered-deny",
        SessionCommandPayload::RespondApproval {
            request_id: "approval-request".into(),
            decision: luna_proto::ApprovalDecision::AllowOnce,
        },
    );
    wait_for(
        || {
            command_status(&core, "cmd-recovered-deny")
                .is_some_and(|(status, _)| status != SessionCommandStatus::Pending)
        },
        "recovered approval safely closes",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-recovered-deny"),
        Some((
            SessionCommandStatus::Applied,
            Some("provider disconnected; approval denied safely".into())
        ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_permission_change_keeps_active_effective_until_replacement_starts() {
    struct PermissionTransitionHarness {
        seen: Arc<std::sync::Mutex<Vec<RunRequest>>>,
        steers: Arc<std::sync::atomic::AtomicUsize>,
        start: Arc<tokio::sync::Semaphore>,
        finish: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait]
    impl Harness for PermissionTransitionHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Permission transition"
        }
        fn supports_steering(&self) -> bool {
            true
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::StepBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let run_number = {
                let mut seen = self.seen.lock().unwrap();
                seen.push(request);
                seen.len()
            };
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let start = self.start.clone();
            let finish = self.finish.clone();
            let interrupt = controls.interrupt.clone();
            let mut steering = controls.steering;
            let steers = self.steers.clone();
            tokio::spawn(async move {
                let permit = start.acquire().await.unwrap();
                permit.forget();
                let _ = tx
                    .send(Ok(AgentEvent::SessionStarted {
                        harness: HarnessId::Mock,
                        model: "mock-1".into(),
                        tools: vec![],
                        cwd: "/tmp".into(),
                        session_id: format!("permission-{run_number}"),
                        assistant_message_id: format!("permission-answer-{run_number}"),
                    }))
                    .await;
                loop {
                    tokio::select! {
                        permit = finish.acquire() => { permit.unwrap().forget(); break; }
                        Some(mut message) = steering.recv() => {
                            steers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if let Some(ack) = message.acceptance.take() { let _ = ack.send(SteerAcceptance::Accepted); }
                        }
                    }
                }
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
                interrupt.cancelled().await;
                let _ = tx.send(Ok(done(DoneStatus::Interrupted))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    for (case, initial, requested) in [
        (
            "restrict",
            luna_proto::PermissionMode::Full,
            luna_proto::PermissionMode::Ask,
        ),
        (
            "escalate",
            luna_proto::PermissionMode::Ask,
            luna_proto::PermissionMode::Full,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let seen: Arc<std::sync::Mutex<Vec<RunRequest>>> = Default::default();
        let steers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Semaphore::new(0));
        let finish = Arc::new(tokio::sync::Semaphore::new(0));
        let core = assemble(
            dir.path(),
            Arc::new(PermissionTransitionHarness {
                seen: seen.clone(),
                steers: steers.clone(),
                start: start.clone(),
                finish: finish.clone(),
            }),
        );
        core.workspace
            .create_space("execution-space", &core.device_id, "/tmp", None, false)
            .unwrap();
        core.workspace
            .create_chat(
                CHAT,
                Some("execution-space"),
                Some(&core.device_id),
                Some(luna_proto::ChatConfig {
                    harness: HarnessId::Mock,
                    provider_account_id: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: if initial == luna_proto::PermissionMode::Full {
                        SandboxLevel::DangerFullAccess
                    } else {
                        SandboxLevel::WorkspaceWrite
                    },
                    permission_mode: initial,
                    effective_permission_mode: initial,
                    permission_ceiling_source: None,
                    permission_degradation_reason: None,
                    permission_audit: Vec::new(),
                }),
                Some("/tmp".into()),
            )
            .unwrap();
        core.workspace.rename_chat(CHAT, "Pre-titled").unwrap();
        let client = luna_rpc::memory_client(core.rpc_service());

        let mut first = run_request(&format!("{case}-first"));
        first.permission_mode = initial;
        first.model_options.insert(
            "lunaProviderAuthInstance".into(),
            "INTERNAL_AUTH_SENTINEL".into(),
        );
        first.model_options.insert(
            "__private_runtime".into(),
            "INTERNAL_PROMPT_SENTINEL".into(),
        );
        core.sessions
            .dispatch(CHAT, HarnessId::Mock, first, None)
            .await
            .unwrap();
        wait_for(
            || seen.lock().unwrap().len() == 1,
            "first permission runtime invocation",
        )
        .await;
        start.add_permits(1);
        wait_for(
            || {
                core.workspace
                    .chat_harness_session(CHAT)
                    .is_some_and(|(id, _)| id == "permission-1")
            },
            "first permission runtime start",
        )
        .await;

        let mut execution_watch = client
            .subscribe("WatchChats", serde_json::json!({}))
            .await
            .unwrap();
        let before_execution = client
            .call(
                luna_rpc::methods::GET_RUNTIME_CONTRACT,
                serde_json::json!({"taskId": CHAT}),
            )
            .await
            .unwrap();
        assert_eq!(
            before_execution["execution"]["pending"],
            serde_json::json!([]),
            "inherited model/account defaults do not create a pending change"
        );
        assert_eq!(
            before_execution["execution"]["running"]["model"], "mock-1",
            "the provider-observed default model is visible"
        );
        assert_eq!(
            before_execution["execution"]["running"]["modelOptions"],
            serde_json::json!({})
        );
        assert!(
            !before_execution.to_string().contains("INTERNAL_"),
            "private runtime values are redacted"
        );

        client
            .call(
                luna_rpc::methods::MUTATE,
                serde_json::json!({
                    "op": "setThreadPermissionMode",
                    "chatId": CHAT,
                    "mode": requested,
                    "actor": "permission-transition-test",
                }),
            )
            .await
            .unwrap();
        let staged = core.workspace.chat(CHAT).unwrap().unwrap().config.unwrap();
        assert_eq!(staged.permission_mode, requested, "{case}: request staged");
        assert_eq!(
            staged.effective_permission_mode, initial,
            "{case}: active provider policy stays authoritative"
        );
        assert!(
            staged
                .permission_audit
                .last()
                .is_some_and(|entry| entry.provider_mapping.starts_with("pending:")),
            "{case}: request is audited as pending"
        );

        // Execution observations must retain the old policy until turnover,
        // and a new steer cannot silently bypass the pending change.
        let pending_execution = client
            .call(
                luna_rpc::methods::GET_RUNTIME_CONTRACT,
                serde_json::json!({"taskId": CHAT}),
            )
            .await
            .unwrap();
        if let Some(output) = std::env::var_os("LUNA_EXECUTION_EVIDENCE") {
            std::fs::create_dir_all(&output).unwrap();
            std::fs::write(
                std::path::Path::new(&output).join(format!("{case}-pending.json")),
                serde_json::to_vec_pretty(&pending_execution).unwrap(),
            )
            .unwrap();
        }
        let execution = &pending_execution["execution"];
        assert_eq!(
            execution["schemaVersion"], 1,
            "{case}: versioned execution snapshot"
        );
        assert_eq!(
            execution["requested"]["permissionMode"],
            serde_json::to_value(requested).unwrap()
        );
        assert_eq!(
            execution["running"]["permissionMode"],
            serde_json::to_value(initial).unwrap()
        );
        let next_sandbox = if requested == luna_proto::PermissionMode::Full {
            SandboxLevel::DangerFullAccess
        } else {
            SandboxLevel::WorkspaceWrite
        };
        assert_eq!(
            execution["requested"]["sandbox"],
            serde_json::to_value(next_sandbox).unwrap(),
            "requested sandbox follows the same permission resolution as dispatch, not stale legacy storage"
        );
        assert_eq!(execution["phase"], "running");
        assert!(
            execution["pending"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("permission"))
        );
        assert_eq!(execution["transition"]["kind"], "restartAndResume");
        assert_eq!(execution["transition"]["waitsForTurnEnd"], true);
        assert_eq!(execution["steer"]["kind"], "unavailable");
        let published_execution = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = execution_watch
                    .recv()
                    .await
                    .expect("execution watch remains open");
                if let Some(row) = rows
                    .as_array()
                    .and_then(|rows| rows.iter().find(|row| row["id"] == CHAT))
                    && row["execution"]["pending"]
                        .as_array()
                        .is_some_and(|pending| pending.contains(&serde_json::json!("permission")))
                {
                    break row["execution"].clone();
                }
            }
        })
        .await
        .expect("the chat watch publishes pending settings");
        assert_eq!(published_execution["running"], execution["running"]);
        assert_eq!(published_execution["requested"], execution["requested"]);
        assert!(!published_execution.to_string().contains("INTERNAL_"));
        assert!(
            !execution.to_string().contains("permission-1"),
            "no native session IDs in diagnostics"
        );
        let steer = core
            .sessions
            .steer(
                CHAT,
                "must not use the obsolete policy",
                Some(format!("{case}-pending-steer")),
            )
            .await
            .unwrap();
        assert_eq!(
            format!("{steer:?}"),
            "ConfigurationChanged",
            "{case}: changed settings block native steering before delivery"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "diagnostics and rejected steering start no provider"
        );
        assert_eq!(
            steers.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no prompt reached the old runtime mailbox"
        );

        finish.add_permits(1);
        wait_for(
            || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
            "first permission turn boundary",
        )
        .await;

        let handle = core.doc_host.open(CHAT).unwrap();
        queue_as_owner(
            &core,
            handle.doc(),
            &format!("{case}-follow-up"),
            SessionCommandPayload::Steer {
                attachments: Vec::new(),
                input_items: Vec::new(),
                prompt: format!("{case}-second"),
                message_id: Some(format!("{case}-next-turn")),
            },
        );
        wait_for(
            || seen.lock().unwrap().len() == 2,
            "replacement permission runtime invocation",
        )
        .await;
        let starting = core.workspace.chat(CHAT).unwrap().unwrap().config.unwrap();
        assert_eq!(starting.permission_mode, requested);
        assert_eq!(
            starting.effective_permission_mode, initial,
            "{case}: constructing a replacement is not proof it started"
        );
        let applied_request = seen.lock().unwrap()[1].clone();
        assert_eq!(
            applied_request
                .permission_policy
                .as_ref()
                .map(|policy| policy.effective_mode),
            Some(requested),
            "{case}: replacement provider receives the requested policy"
        );

        start.add_permits(1);
        wait_for(
            || {
                core.workspace
                    .chat(CHAT)
                    .unwrap()
                    .and_then(|chat| chat.config)
                    .is_some_and(|config| config.effective_permission_mode == requested)
            },
            "replacement permission policy publication",
        )
        .await;
        let applied = core.workspace.chat(CHAT).unwrap().unwrap().config.unwrap();
        assert_eq!(applied.permission_mode, requested);
        assert_eq!(applied.effective_permission_mode, requested);
        assert!(
            applied
                .permission_audit
                .last()
                .is_some_and(|entry| !entry.provider_mapping.starts_with("pending:")),
            "{case}: SessionStarted records the applied policy"
        );
        let applied_execution = client
            .call(
                luna_rpc::methods::GET_RUNTIME_CONTRACT,
                serde_json::json!({"taskId": CHAT}),
            )
            .await
            .unwrap();
        assert_eq!(
            applied_execution["execution"]["running"]["permissionMode"],
            serde_json::to_value(requested).unwrap()
        );
        assert_eq!(
            applied_execution["execution"]["pending"],
            serde_json::json!([])
        );
        assert_eq!(
            applied_execution["execution"]["transition"]["kind"],
            "reuse"
        );
        if let Some(output) = std::env::var_os("LUNA_EXECUTION_EVIDENCE") {
            std::fs::write(
                std::path::Path::new(&output).join(format!("{case}-applied.json")),
                serde_json::to_vec_pretty(&applied_execution).unwrap(),
            )
            .unwrap();
        }

        finish.add_permits(1);
        wait_for(
            || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
            "replacement permission turn boundary",
        )
        .await;

        // Idle changes retire the parked runtime under the dispatch gate and
        // may publish immediately: no provider is left enforcing the old mode.
        client
            .call(
                luna_rpc::methods::MUTATE,
                serde_json::json!({
                    "op": "setThreadPermissionMode",
                    "chatId": CHAT,
                    "mode": initial,
                    "actor": "permission-transition-test-idle",
                }),
            )
            .await
            .unwrap();
        let idle_applied = core.workspace.chat(CHAT).unwrap().unwrap().config.unwrap();
        assert_eq!(idle_applied.permission_mode, initial);
        assert_eq!(
            idle_applied.effective_permission_mode, initial,
            "{case}: idle retirement is an immediate safe apply boundary"
        );
        let idle_execution = client
            .call(
                luna_rpc::methods::GET_RUNTIME_CONTRACT,
                serde_json::json!({"taskId": CHAT}),
            )
            .await
            .unwrap();
        assert!(
            idle_execution["execution"]["running"].is_null(),
            "retired runtimes cannot remain live in diagnostics"
        );
        core.shutdown().await;
    }
}

#[tokio::test]
async fn unsupported_auto_never_runs_as_hidden_ask() {
    /// An installed OpenCode, so dispatch reaches the permission guard instead
    /// of stopping at the missing provider. Counts every run it is asked for.
    #[derive(Default)]
    struct InstalledOpenCode {
        runs: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Harness for InstalledOpenCode {
        fn id(&self) -> HarnessId {
            HarnessId::Opencode
        }
        fn display_name(&self) -> &str {
            "OpenCode"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            _controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(HarnessError::Protocol("OpenCode must not start".into()))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let opencode = Arc::new(InstalledOpenCode::default());
    let core = assemble(dir.path(), opencode.clone());
    let mut request = run_request("must not run");
    request.harness = Some(HarnessId::Opencode);
    request.permission_mode = luna_proto::PermissionMode::Auto;
    let error = core
        .sessions
        .dispatch(CHAT, HarnessId::Opencode, request, None)
        .await
        .expect_err("OpenCode Auto must not degrade to hidden Ask");
    assert!(error.to_string().contains("hidden Ask"), "{error}");
    assert_eq!(
        opencode.runs.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the provider is never started"
    );
    assert!(core.sessions.session_status(CHAT).is_none());
}

/// Nonblocking questions are already complete provider tool calls: "answer
/// later" leaves the card open, a later answer uses normal prompt delivery
/// tagged with the card id, the same answer again is idempotent, and a
/// different later answer is delivered as an amendment.
#[tokio::test(flavor = "multi_thread")]
async fn async_question_skip_and_late_answer_use_distinct_durable_paths() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(ScriptedHarness {
            script: vec![
                AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: "synthetic".into(),
                    tools: vec![],
                    cwd: "/tmp".into(),
                    session_id: "synthetic-session".into(),
                    assistant_message_id: "assistant-1".into(),
                },
                AgentEvent::TextDelta {
                    text: "Which option?".into(),
                },
                AgentEvent::AsyncQuestions {
                    message_id: "call-1".into(),
                    questions: vec![luna_proto::AsyncUserInputQuestion {
                        title: "Which option?".into(),
                        options: Some(vec!["A".into(), "B".into()]),
                        ..Default::default()
                    }],
                },
                done(DoneStatus::Completed),
            ],
            step_delay: Duration::from_millis(1),
            hang_until_interrupt: false,
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "async-run",
        SessionCommandPayload::Run {
            request: run_request("start"),
            message_id: "start-message".into(),
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.parts.iter().any(|part|
        matches!(part, MessagePart::AsyncQuestion { id, resolved: false, .. } if id == "aq-call-1")
    )
            })
        },
        "async card published",
    )
    .await;
    wait_for(
        || {
            command_status(&core, "async-run")
                .is_some_and(|(status, _)| status == SessionCommandStatus::Applied)
        },
        "first run applied",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.status == Some(MessageStatus::Complete)
                    && e.parts.iter().any(|part| {
                        matches!(part,
            MessagePart::AsyncQuestion { id, .. } if id == "aq-call-1")
                    })
            })
        },
        "async run settled",
    )
    .await;

    queue_as_owner(
        &core,
        handle.doc(),
        "skip-call-1",
        SessionCommandPayload::RespondAsyncQuestion {
            message_id: "aq-call-1".into(),
            answers: vec![],
        },
    );
    wait_for(
        || {
            command_status(&core, "skip-call-1")
                .is_some_and(|(status, _)| status == SessionCommandStatus::Applied)
        },
        "skip applied",
    )
    .await;
    assert!(entries(&core).iter().any(|e| {
        e.parts.iter().any(|part| {
            matches!(part, MessagePart::AsyncQuestion { id, resolved: false, answers, .. }
            if id == "aq-call-1" && answers.is_empty())
        })
    }));
    assert_eq!(
        command_status(&core, "skip-call-1").unwrap().1.as_deref(),
        Some("question left open")
    );

    handle
        .doc()
        .push_message(&SessionMessageEntry {
            id: "later-async".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::AsyncQuestion {
                id: "aq-call-2".into(),
                questions: vec![luna_proto::AsyncUserInputQuestion {
                    title: "Choose a later option".into(),
                    options: Some(vec!["A".into(), "B".into()]),
                    ..Default::default()
                }],
                resolved: false,
                answers: Vec::new(),
            }],
            created_at: chrono::Utc::now().timestamp_millis(),
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "answer-call-2",
        SessionCommandPayload::RespondAsyncQuestion {
            message_id: "aq-call-2".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "0".into(),
                labels: vec!["B".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "answer-call-2")
                .is_some_and(|(status, _)| status == SessionCommandStatus::Applied)
        },
        "late answer applied",
    )
    .await;
    assert!(entries(&core).iter().any(|e| {
        e.parts.iter().any(|part|
        matches!(part, MessagePart::AsyncQuestion { id, resolved: true, .. } if id == "aq-call-2")
    )
    }));
    assert!(entries(&core).iter().any(|e| {
        e.role == MessageRole::User && e.parts.iter().any(|part|
        matches!(part, MessagePart::Text { text, .. }
            if text.contains("Choose a later option: B") && text.contains("[luna-question:aq-call-2]"))
    )
    }));
    queue_as_owner(
        &core,
        handle.doc(),
        "duplicate-answer-call-2",
        SessionCommandPayload::RespondAsyncQuestion {
            message_id: "aq-call-2".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "0".into(),
                labels: vec!["B".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "duplicate-answer-call-2")
                .is_some_and(|(status, _)| status == SessionCommandStatus::Applied)
        },
        "duplicate answer is idempotent",
    )
    .await;
    assert_eq!(
        entries(&core)
            .iter()
            .filter(|e| {
                e.role == MessageRole::User && e.parts.iter().any(|part|
        matches!(part, MessagePart::Text { text, .. } if text.contains("Choose a later option: B"))
    )
            })
            .count(),
        1
    );
    // This case exercises an amendment after the previous answer's turn.
    // ScriptedHarness does not consume live steering: under concurrent test
    // load that turn can still be active after the duplicate receipt lands.
    // Keep the duplicate timing above; wait for actual runtime settlement
    // before the late amendment. The boundary test below covers live steering.
    wait_for(
        || !core.sessions.has_live_run(CHAT),
        "prior answer turn settled before late amendment",
    )
    .await;
    queue_as_owner(
        &core,
        handle.doc(),
        "amend-call-2",
        SessionCommandPayload::RespondAsyncQuestion {
            message_id: "aq-call-2".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "0".into(),
                labels: vec!["A".into()],
            }],
        },
    );
    wait_for(
        || {
            let receipt = command_status(&core, "amend-call-2");
            if let Some((SessionCommandStatus::Rejected, reason)) = &receipt {
                panic!("late amendment was rejected: {reason:?}");
            }
            receipt.is_some_and(|(status, _)| status == SessionCommandStatus::Applied)
        },
        "amended answer applied",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.role == MessageRole::User && e.parts.iter().any(|part|
            matches!(part, MessagePart::Text { text, .. }
                if text.starts_with("I changed my answer") && text.contains("Choose a later option: A"))
        )
            })
        },
        "amendment delivered as a new message",
    )
    .await;
    assert!(entries(&core).iter().any(|e| {
        e.parts.iter().any(|part| {
            matches!(part, MessagePart::AsyncQuestion { id, answers, .. }
            if id == "aq-call-2" && answers.len() == 2)
        })
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_answer_steer_waits_for_in_flight_tool_boundary() {
    struct BoundaryHarness {
        release_tool: Arc<tokio::sync::Notify>,
        tool_was_interrupted: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait]
    impl Harness for BoundaryHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Async boundary"
        }
        fn supports_steering(&self) -> bool {
            true
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::StepBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            mut controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let release = self.release_tool.clone();
            let interrupted = self.tool_was_interrupted.clone();
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(AgentEvent::AsyncQuestions {
                        message_id: "boundary-call".into(),
                        questions: vec![luna_proto::AsyncUserInputQuestion {
                            title: "Which option?".into(),
                            options: Some(vec!["A".into(), "B".into()]),
                            ..Default::default()
                        }],
                    }))
                    .await;
                let _ = tx
                    .send(Ok(AgentEvent::ToolCall {
                        id: "tool-in-flight".into(),
                        call: ToolCall::WriteFile {
                            path: "/tmp/synthetic-only".into(),
                            content: None,
                        },
                    }))
                    .await;
                let Some(message) = controls.steering.recv().await else {
                    return;
                };
                assert!(message.prompt.contains("Which option?: B"));
                if let Some(acceptance) = message.acceptance {
                    let _ = acceptance.send(SteerAcceptance::Accepted);
                }
                release.notified().await;
                interrupted.store(
                    controls.interrupt.is_cancelled(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                let _ = tx
                    .send(Ok(AgentEvent::ToolResult {
                        id: "tool-in-flight".into(),
                        is_error: false,
                        output: None,
                        diff: None,
                    }))
                    .await;
                let _ = tx
                    .send(Ok(AgentEvent::Steered {
                        user_message_id: None,
                        assistant_message_id: None,
                        next_assistant_message_id: None,
                    }))
                    .await;
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: "after tool boundary".into(),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(BoundaryHarness {
            release_tool: release_tool.clone(),
            tool_was_interrupted: interrupted.clone(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "boundary-run",
        SessionCommandPayload::Run {
            request: run_request("start"),
            message_id: "boundary-start".into(),
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.parts.iter().any(|part|
        matches!(part, MessagePart::Tool { id, resolved: false, .. } if id == "tool-in-flight")
    )
            })
        },
        "in-flight tool published",
    )
    .await;
    queue_as_owner(
        &core,
        handle.doc(),
        "boundary-answer",
        SessionCommandPayload::RespondAsyncQuestion {
            message_id: "aq-boundary-call".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "0".into(),
                labels: vec!["B".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "boundary-answer")
                .is_some_and(|(status, _)| status == SessionCommandStatus::Applied)
        },
        "answer accepted while tool runs",
    )
    .await;
    assert!(entries(&core).iter().any(|entry| {
        entry.parts.iter().any(|part|
        matches!(part, MessagePart::Tool { id, resolved: false, .. } if id == "tool-in-flight")
    )
    }));
    assert!(!entries(&core).iter().any(|entry| {
        entry.parts.iter().any(|part|
        matches!(part, MessagePart::Text { text, .. } if text.contains("after tool boundary"))
    )
    }));
    release_tool.notify_one();
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.parts.iter().any(|part|
        matches!(part, MessagePart::Text { text, .. } if text.contains("after tool boundary"))
    )
            })
        },
        "answer consumed after tool boundary",
    )
    .await;
    assert!(!interrupted.load(std::sync::atomic::Ordering::SeqCst));
}

/// Resilience: a RespondInput whose id matches no pending request is REJECTED
/// with a resolution (never silently dropped), the question stays live (the
/// panel persists), and a subsequent correct answer still resumes the run —
/// a wrong answer can never brick the session.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_id_respond_is_rejected_and_correct_answer_still_resumes() {
    struct AskingHarness;
    #[async_trait]
    impl Harness for AskingHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Asking"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
            tokio::spawn(async move {
                let answers = (controls.request_input)(vec![luna_proto::UserInputQuestion {
                    id: "q1".into(),
                    header: "Pick".into(),
                    question: "Which one?".into(),
                    options: vec!["a".into(), "b".into()],
                    multi_select: false,
                    mcp: None,
                    approval: None,
                    input: None,
                }])
                .await
                .unwrap_or_default();
                let picked = answers
                    .first()
                    .and_then(|a| a.labels.first().cloned())
                    .unwrap_or_else(|| "none".into());
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: format!("picked {picked}"),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(AskingHarness));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-wrong",
        SessionCommandPayload::Run {
            request: run_request("ask me"),
            message_id: "m-1".into(),
        },
    );
    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingInput)
        },
        "awaiting input",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.parts.iter().any(|p| {
                    matches!(
                        p,
                        MessagePart::Input {
                            resolved: false,
                            ..
                        }
                    )
                })
            })
        },
        "input part in doc",
    )
    .await;

    // A wrong-id answer: rejected with a resolution, question still live.
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-answer-bogus",
        SessionCommandPayload::RespondInput {
            request_id: "bogus-id".into(),
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "q1".into(),
                labels: vec!["a".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&core, "cmd-answer-bogus")
                .is_some_and(|(s, _)| s != SessionCommandStatus::Pending)
        },
        "bogus answer processed",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-answer-bogus"),
        Some((
            SessionCommandStatus::Rejected,
            Some("no pending input request".into())
        ))
    );
    // The run is still waiting and the part is still unresolved — the
    // QuestionPanel keeps presenting the real request.
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::AwaitingInput)
    );
    let request_id = entries(&core)
        .iter()
        .find_map(|e| {
            e.parts.iter().find_map(|p| match p {
                MessagePart::Input {
                    request_id,
                    resolved: false,
                    ..
                } => Some(request_id.clone()),
                _ => None,
            })
        })
        .expect("question still live after rejected answer");

    // The correct answer still resumes and completes the run.
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-answer-right",
        SessionCommandPayload::RespondInput {
            request_id,
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "q1".into(),
                labels: vec!["b".into()],
            }],
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.status == Some(MessageStatus::Complete)
                    && e.parts
                        .iter()
                        .any(|p| matches!(p, MessagePart::Text { text, .. } if text == "picked b"))
            })
        },
        "answered turn to complete",
    )
    .await;
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Idle)
    );
}

/// Resilience: interrupting a run that is BLOCKED on a question unparks the
/// harness immediately (the pending resolver is failed with empty answers),
/// the entry settles `aborted`, the chip flips terminal (never dangles
/// unresolved), and the next run works — a blocked question can never brick
/// the session.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_unblocks_a_run_awaiting_input() {
    struct BlockingHarness;
    #[async_trait]
    impl Harness for BlockingHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Blocking"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
            if request.prompt == "second run" {
                // The post-interrupt turn: completes immediately.
                tokio::spawn(async move {
                    let _ = tx
                        .send(Ok(AgentEvent::TextDelta {
                            text: "second done".into(),
                        }))
                        .await;
                    let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
                });
            } else {
                let interrupt = controls.interrupt.clone();
                tokio::spawn(async move {
                    // Blocks on the question; an interrupt fails the resolver
                    // (empty answers) and cancels the token — like a real CLI
                    // being torn down, the stream then ends WITHOUT a Done.
                    let _ = (controls.request_input)(vec![luna_proto::UserInputQuestion {
                        id: "q1".into(),
                        header: "Pick".into(),
                        question: "Which one?".into(),
                        options: vec!["a".into(), "b".into()],
                        multi_select: false,
                        mcp: None,
                        approval: None,
                        input: None,
                    }])
                    .await;
                    interrupt.cancelled().await;
                    drop(tx);
                });
            }
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(BlockingHarness));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-block",
        SessionCommandPayload::Run {
            request: run_request("ask and block"),
            message_id: "m-1".into(),
        },
    );
    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingInput)
        },
        "awaiting input",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.parts.iter().any(|p| {
                    matches!(
                        p,
                        MessagePart::Input {
                            resolved: false,
                            ..
                        }
                    )
                })
            })
        },
        "input part in doc",
    )
    .await;

    // Interrupt while blocked: settles promptly (well under the 3s grace —
    // the unparked resolver lets the harness wind down on its own).
    let start = std::time::Instant::now();
    core.sessions.interrupt(CHAT).await.unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(3),
        "interrupt settled via the unparked resolver, not the grace timeout"
    );
    wait_for(
        || {
            entries_now(&core)
                .iter()
                .any(|e| e.status == Some(MessageStatus::Aborted))
        },
        "entry stamped aborted",
    )
    .await;
    // The chip is terminal — no dangling unresolved question survives the run.
    assert!(entries(&core).iter().all(|e| {
        e.parts.iter().all(|p| {
            !matches!(
                p,
                MessagePart::Input {
                    resolved: false,
                    ..
                }
            )
        })
    }));

    // And the session is usable: the next run completes.
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-second",
        SessionCommandPayload::Run {
            request: run_request("second run"),
            message_id: "m-2".into(),
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.status == Some(MessageStatus::Complete)
                    && e.parts.iter().any(
                        |p| matches!(p, MessagePart::Text { text, .. } if text == "second done"),
                    )
            })
        },
        "second run to complete",
    )
    .await;
}

/// Regression (the "nothing happened after I answered" bug): a harness that
/// emits its OWN `InputRequested` (keyed by its internal id — Claude's
/// control-request id) *and* asks through `RunControls::request_input` used to
/// fold TWO input parts into the doc. The UI answers the LAST unresolved part;
/// the harness-emitted twin's id was unknown to `respond_input`'s pending map,
/// so the RespondInput doc command was rejected and the run never resumed.
/// The engine now drops harness-emitted `InputRequested` events (the input
/// bridge is the sole authority), so exactly one — answerable — part folds.
#[tokio::test(flavor = "multi_thread")]
async fn harness_emitted_input_twin_is_dropped_and_answer_resumes() {
    struct DoubleEmitHarness;
    #[async_trait]
    impl Harness for DoubleEmitHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "DoubleEmit"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
            tokio::spawn(async move {
                let question = luna_proto::UserInputQuestion {
                    id: "q1".into(),
                    header: "Pick".into(),
                    question: "Which one?".into(),
                    options: vec!["a".into(), "b".into()],
                    multi_select: false,
                    mcp: None,
                    approval: None,
                    input: None,
                };
                // The pre-fix Claude/Codex shape: surface the question under
                // the harness's own id BEFORE asking through the bridge.
                let _ = tx
                    .send(Ok(AgentEvent::InputRequested {
                        request_id: "claude-ctrl-1".into(),
                        questions: vec![question.clone()],
                    }))
                    .await;
                let answers = (controls.request_input)(vec![question])
                    .await
                    .unwrap_or_default();
                let picked = answers
                    .first()
                    .and_then(|a| a.labels.first().cloned())
                    .unwrap_or_else(|| "none".into());
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: format!("picked {picked}"),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(DoubleEmitHarness));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-twin",
        SessionCommandPayload::Run {
            request: run_request("ask me twice"),
            message_id: "m-1".into(),
        },
    );

    wait_for(
        || {
            core.sessions.session_status(CHAT).map(|s| s.status)
                == Some(SessionStatus::AwaitingInput)
        },
        "awaiting input",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.parts.iter().any(|p| {
                    matches!(
                        p,
                        MessagePart::Input {
                            resolved: false,
                            ..
                        }
                    )
                })
            })
        },
        "input part in doc",
    )
    .await;

    // Exactly ONE input part folded, and not under the harness's own id.
    let input_ids: Vec<String> = entries(&core)
        .iter()
        .flat_map(|e| {
            e.parts.iter().filter_map(|p| match p {
                MessagePart::Input { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(input_ids.len(), 1, "one chip, not a twin: {input_ids:?}");
    assert_ne!(input_ids[0], "claude-ctrl-1");

    // Answer the LAST unresolved part — exactly what the QuestionPanel does.
    let request_id = entries(&core)
        .iter()
        .rev()
        .find_map(|e| {
            e.parts.iter().rev().find_map(|p| match p {
                MessagePart::Input {
                    request_id,
                    resolved: false,
                    ..
                } => Some(request_id.clone()),
                _ => None,
            })
        })
        .unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-answer-twin",
        SessionCommandPayload::RespondInput {
            request_id,
            answers: vec![luna_proto::UserInputAnswer {
                question_id: "q1".into(),
                labels: vec!["a".into()],
            }],
        },
    );

    // The run resumes and completes; the chip flips to resolved.
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.status == Some(MessageStatus::Complete)
                    && e.parts
                        .iter()
                        .any(|p| matches!(p, MessagePart::Text { text, .. } if text == "picked a"))
            })
        },
        "answered turn to complete",
    )
    .await;
    assert_eq!(
        command_status(&core, "cmd-answer-twin"),
        Some((SessionCommandStatus::Applied, None))
    );
    assert!(entries(&core).iter().any(|e| {
        e.parts
            .iter()
            .any(|p| matches!(p, MessagePart::Input { resolved: true, .. }))
    }));
    // The run task writes the Complete entry BEFORE settling the status row —
    // wait for the transition instead of asserting the instant in between.
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "session to settle idle",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Attachments (round 17): chunked upload → durable path → Run carrying both
// the prompt-embedded refs (the persisted transport) and the staged paths.
// ---------------------------------------------------------------------------

/// Delegates to a scripted mock but records every RunRequest the engine hands
/// over (the chat run AND the auto-title run share the harness) — proves
/// `attachments` survives doc-queue → executor → harness.
struct CapturingHarness {
    script: Vec<AgentEvent>,
    seen: Arc<std::sync::Mutex<Vec<RunRequest>>>,
}

#[async_trait]
impl Harness for CapturingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Capturing"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.seen.lock().unwrap().push(request.clone());
        MockHarness {
            script: self.script.clone(),
        }
        .run(request, controls)
        .await
    }
}

#[tokio::test]
async fn attachment_upload_then_run_threads_refs_and_paths() {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let dir = tempfile::tempdir().unwrap();
    let seen: Arc<std::sync::Mutex<Vec<RunRequest>>> = Default::default();
    let core = assemble(
        dir.path(),
        Arc::new(CapturingHarness {
            script: mock_script(),
            seen: seen.clone(),
        }),
    );
    let client = luna_rpc::memory_client(core.rpc_service());

    // Chunked upload exactly as the composer sends it: base64 split across
    // positional UploadChunk slots, then UploadCommit → the durable path.
    let payload: Vec<u8> = (0..=255u8).cycle().take(9_001).collect();
    let encoded = b64.encode(&payload);
    let (first, second) = encoded.split_at(encoded.len() / 2);
    for (seq, data) in [(0, first), (1, second)] {
        client
            .call(
                luna_rpc::methods::UPLOAD_CHUNK,
                serde_json::json!({ "uploadId": "e2e-att", "seq": seq, "data": data }),
            )
            .await
            .expect("UploadChunk");
    }
    let committed = client
        .call(
            luna_rpc::methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": "e2e-att", "fileName": "red.png" }),
        )
        .await
        .expect("UploadCommit");
    let path = committed["path"].as_str().expect("path").to_string();
    assert_eq!(
        std::fs::read(&path).expect("durable upload file"),
        payload,
        "committed file holds the exact reassembled bytes"
    );

    // Run with the luna `withAttachments` transport: refs embedded in the
    // prompt text (this is what persists), paths on the additive field.
    let prompt = format!(
        "what color is this?\n\nAttached images (local files — open them to view):\n- {path}"
    );
    let mut request = run_request(&prompt);
    request.attachments = vec![path.clone()];
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-att-1",
        SessionCommandPayload::Run {
            request,
            message_id: "msg-att-1".into(),
        },
    );
    wait_for(
        || {
            entries_now(&core).iter().any(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
        },
        "assistant entry to complete",
    )
    .await;

    // Doc user entry: the message text carries the refs verbatim (render-back
    // parses them into thumbnails).
    let all = entries(&core);
    assert_eq!(all[0].id, "msg-att-1");
    assert_eq!(all[0].role, MessageRole::User);
    match &all[0].parts[0] {
        MessagePart::Text { text, .. } => {
            assert!(text.contains("Attached images (local files"));
            assert!(text.contains(&path));
        }
        other => panic!("unexpected user part {other:?}"),
    }

    // The harness saw the staged paths on the request itself (the chat run —
    // NOT the auto-title run, which fires at dispatch now, embeds the user
    // prompt in its wrapper, and legitimately carries no attachments).
    let requests = seen.lock().unwrap().clone();
    let chat_run = requests
        .iter()
        .find(|r| r.prompt.contains("what color is this?") && !r.prompt.contains("word title"))
        .expect("chat run reached the harness");
    assert_eq!(chat_run.attachments, vec![path.clone()]);
    assert!(chat_run.prompt.contains(&path));

    // Read-back over the same RPC surface the transcript uses.
    let chunk = client
        .call(
            luna_rpc::methods::READ_ATTACHMENT_CHUNK,
            serde_json::json!({ "path": path, "offset": 0 }),
        )
        .await
        .expect("ReadAttachmentChunk");
    assert_eq!(chunk["mimeType"], "image/png");
    assert_eq!(chunk["name"], "e2e-att-red.png");
}

/// Real-CLI proof of the image pipeline: upload a tiny solid-red PNG through
/// the chunked RPC path, run claude (haiku) with the staged path on
/// `attachments` + the refs in the prompt, and check the reply names the
/// color — it can only know it by SEEING the inline image block (the sandbox
/// prompt forbids opening the file). Ignored by default: needs an installed,
/// authenticated `claude` CLI and spends real tokens.
/// Run with: `cargo test -p luna-engine --test e2e -- --ignored`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires installed+authenticated claude CLI; spends tokens"]
async fn real_claude_sees_uploaded_image_inline() {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();

    let core = EngineCore::assemble(
        &dir,
        Arc::new(luna_engine::default_registry()),
        HarnessId::ClaudeCode,
        None,
    )
    .expect("engine core assembles");
    // Pre-title the chat so the auto-titler doesn't spend a second model call.
    core.workspace
        .create_chat(CHAT, None, Some(&core.device_id), None, Some("/tmp".into()))
        .expect("create chat row");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");

    // 8×8 solid-red PNG, uploaded exactly as the composer does.
    const RED_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEklEQVR4nGP8z4AdMOEQH6QSAM1BAQ/oQeJvAAAAAElFTkSuQmCC";
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            luna_rpc::methods::UPLOAD_CHUNK,
            serde_json::json!({ "uploadId": "real-img", "seq": 0, "data": RED_PNG_B64 }),
        )
        .await
        .expect("UploadChunk");
    let committed = client
        .call(
            luna_rpc::methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": "real-img", "fileName": "swatch.png" }),
        )
        .await
        .expect("UploadCommit");
    let path = committed["path"].as_str().expect("path").to_string();
    assert_eq!(
        std::fs::read(&path).expect("committed file"),
        b64.decode(RED_PNG_B64).unwrap()
    );

    let prompt = format!(
        "Without running any tools or opening any files, answer from the attached image alone: \
         what solid color is this image? Reply with exactly one lowercase word.\n\n\
         Attached images (local files — open them to view):\n- {path}"
    );
    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model: Some("haiku".into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.to_string_lossy().to_string(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![path],
        resume: None,
        worktree: None,
    };
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request,
                message_id: "msg-img-1".into(),
            },
        )
        .expect("queue real image run");
    wait_for_within_secs(
        || {
            entries_now(&core).iter().any(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
        },
        "real claude image turn",
        120,
    )
    .await;

    let reply: String = entries(&core)
        .iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .flat_map(|e| e.parts.iter())
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(
        reply.contains("red"),
        "claude should name the image's color; got: {reply:?}"
    );
    core.shutdown().await;
}

async fn wait_for_within_secs<F>(mut predicate: F, what: &str, secs: u64)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Liveness heartbeats: empty reasoning deltas keep the session fresh but
// never reach the journal (redacted thinking + tool-input-generation noise).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_reasoning_deltas_are_heartbeats_not_journal_noise() {
    let mut script = vec![AgentEvent::SessionStarted {
        harness: HarnessId::Mock,
        model: "mock-1".into(),
        tools: vec![],
        cwd: "/tmp".into(),
        session_id: "hs-hb".into(),
        assistant_message_id: "a-hb".into(),
    }];
    // A long "silent" stretch: redacted thinking / input_json_delta windows
    // stream as empty reasoning deltas.
    for _ in 0..40 {
        script.push(AgentEvent::ReasoningDelta {
            text: String::new(),
        });
    }
    script.push(AgentEvent::ReasoningDelta {
        text: "planning".into(),
    });
    script.push(AgentEvent::TextDelta {
        text: "done".into(),
    });
    script.push(AgentEvent::Done {
        status: DoneStatus::Completed,
        result: Some("done".into()),
        error: None,
        session_id: None,
    });
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(MockHarness { script }));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-hb-1",
        SessionCommandPayload::Run {
            request: run_request("hb"),
            message_id: "msg-hb-1".into(),
        },
    );
    wait_for(
        || {
            entries(&core).iter().any(|e| {
                e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete)
            })
        },
        "run completes",
    )
    .await;
    // Journal replay: the 40 empties were filtered; real content survived.
    let replay = core.sessions.subscribe(CHAT, 0).unwrap().0;
    let empties = replay
        .iter()
        .filter(|j| matches!(&j.event, AgentEvent::ReasoningDelta { text } if text.is_empty()))
        .count();
    let nonempty = replay
        .iter()
        .filter(|j| matches!(&j.event, AgentEvent::ReasoningDelta { text } if !text.is_empty()))
        .count();
    assert_eq!(empties, 0, "empty reasoning deltas never reach the journal");
    assert_eq!(nonempty, 1, "real reasoning text is preserved");
    assert!(
        replay
            .iter()
            .any(|j| matches!(&j.event, AgentEvent::TextDelta { text } if text == "done")),
        "text deltas unaffected"
    );
}

#[tokio::test]
async fn parked_session_ignores_trailing_frames_and_stays_idle() {
    // ACP children keep forwarding session/update frames after a turn's Done
    // (late tool_call_updates, flushed text). A parked session must treat
    // them as inert: no Working re-arm (the eternally-running-session bug),
    // no phantom assistant entry.
    let mut script = mock_script();
    script.push(AgentEvent::ToolCall {
        id: "tool-1".into(),
        call: ToolCall::Exec {
            command: "echo late-echo".into(),
        },
    });
    script.push(AgentEvent::TextDelta {
        text: "trailing flush".into(),
    });
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(MockHarness { script }));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-parked",
        SessionCommandPayload::Run {
            request: run_request("go"),
            message_id: "m-parked".into(),
        },
    );

    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "session to complete",
    )
    .await;
    // The trailing frames land right after the park; hold the assertion open
    // past the 120ms flush window to catch a phantom segment or a Working
    // re-arm (both are what the old code did).
    let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
    while tokio::time::Instant::now() < deadline {
        assert_eq!(
            core.sessions.session_status(CHAT).map(|s| s.status),
            Some(SessionStatus::Idle),
            "trailing frames must not re-arm Working"
        );
        let all = entries_now(&core);
        assert!(
            all.len() <= 2,
            "trailing frames must not open a phantom entry: {all:#?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let all = entries(&core);
    assert_eq!(all.len(), 2, "user + one assistant entry");
    assert_eq!(all[1].status, Some(MessageStatus::Complete));
}

#[tokio::test]
async fn stale_tool_echo_after_steer_boundary_does_not_split_text() {
    // Adapters re-emit shape-bearing tool_call_updates as full ToolCall
    // events. Once a steer boundary reset the fold, such an echo for a
    // PRIOR segment's tool must not mint a chip mid-text in the new segment
    // (the mid-word transcript splits).
    let full_output = "completed before follow-up\n".repeat(80);
    let script = vec![
        AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: "/tmp".into(),
            session_id: "hs-steer".into(),
            assistant_message_id: "a-1".into(),
        },
        AgentEvent::TextDelta {
            text: "part one".into(),
        },
        AgentEvent::ToolCall {
            id: "tool-long".into(),
            call: ToolCall::Exec {
                command: "sleep 60".into(),
            },
        },
        AgentEvent::Steered {
            user_message_id: None,
            assistant_message_id: Some("a-1".into()),
            next_assistant_message_id: Some("a-2".into()),
        },
        AgentEvent::TextDelta {
            text: "part ".into(),
        },
        // The long-running exec from segment one completes mid-stream of the
        // next segment: a shape-bearing echo plus its result.
        AgentEvent::ToolCall {
            id: "tool-long".into(),
            call: ToolCall::Exec {
                command: "sleep 60".into(),
            },
        },
        AgentEvent::ToolResult {
            id: "tool-long".into(),
            is_error: false,
            output: Some(full_output.clone()),
            diff: Some(luna_proto::ToolDiff {
                path: "synthetic.txt".into(),
                old_text: Some("before\n".into()),
                new_text: "after\n".into(),
                unified_diff: None,
            }),
        },
        AgentEvent::TextDelta { text: "two".into() },
        done(DoneStatus::Completed),
    ];
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(MockHarness { script }));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-echo",
        SessionCommandPayload::Run {
            request: run_request("go"),
            message_id: "m-echo".into(),
        },
    );

    wait_for(
        || {
            entries_now(&core).len() == 3
                && core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle)
        },
        "both segments to land",
    )
    .await;
    let all = entries(&core);
    // Segment one: text + the (unresolved-at-boundary) tool chip.
    assert_eq!(all[1].parts.len(), 2, "{:#?}", all[1].parts);
    let MessagePart::Tool {
        resolved,
        output,
        output_ref,
        output_bytes,
        diff_ref,
        diff_stats,
        ..
    } = &all[1].parts[1]
    else {
        panic!("original tool chip missing")
    };
    assert!(*resolved);
    assert!(
        output
            .as_deref()
            .is_some_and(|text| text.contains("completed before follow-up"))
    );
    assert_eq!(output_bytes, &Some(full_output.len() as u64));
    assert_eq!(output_ref.as_deref(), Some("chat-e2e/tool-long"));
    assert_eq!(diff_ref.as_deref(), Some("chat-e2e/tool-long.diff"));
    assert_eq!(diff_stats.as_ref().unwrap()[0].additions, 1);
    assert_eq!(
        core.sessions
            .tool_output(CHAT, "tool-long")
            .unwrap()
            .as_deref(),
        Some(full_output.as_str())
    );
    let restored = SessionDoc::from_snapshot(&handle.doc().export_snapshot().unwrap()).unwrap();
    assert_eq!(
        restored.read_entries().unwrap(),
        all,
        "late result must survive persistence"
    );
    // Segment two: ONE contiguous text part, no spliced chip.
    assert_eq!(
        all[2].parts,
        vec![MessagePart::Text {
            id: "t0".into(),
            text: "part two".into()
        }],
        "stale echo must not split the streaming text"
    );
}

/// The elapsed timer's base (`started_at`) is per user message: a settled
/// session drops it (no reader can resurrect the previous turn's elapsed —
/// the "timer opens at 30:00 on send" bug), and a steer into a PARKED
/// persistent session restamps it fresh for the new turn.
#[tokio::test]
async fn parked_steer_restamps_started_at_and_idle_clears_it() {
    // Steerable harness whose stream stays open after the turn's Done — the
    // engine parks the session — and whose steering mailbox drives turn two.
    struct ParkingHarness;
    #[async_trait]
    impl Harness for ParkingHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Parking"
        }
        fn supports_steering(&self) -> bool {
            true
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::StepBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            mut controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: "turn one".into(),
                    }))
                    .await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
                // Parked. The next steer is turn two.
                if let Some(_msg) = controls.steering.recv().await {
                    let _ = tx
                        .send(Ok(AgentEvent::Steered {
                            user_message_id: None,
                            assistant_message_id: None,
                            next_assistant_message_id: None,
                        }))
                        .await;
                    let _ = tx
                        .send(Ok(AgentEvent::TextDelta {
                            text: "turn two".into(),
                        }))
                        .await;
                    // Hold the turn open so the test's poll observes Working
                    // (the transition is otherwise sub-millisecond).
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
                }
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), Arc::new(ParkingHarness));
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-run-park-timer",
        SessionCommandPayload::Run {
            request: run_request("first"),
            message_id: "m-1".into(),
        },
    );
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "turn one to park",
    )
    .await;
    let parked = core.sessions.session_status(CHAT).unwrap();
    assert_eq!(
        parked.started_at, None,
        "a settled session must drop its timer base"
    );

    let before_steer = chrono::Utc::now();
    queue_as_owner(
        &core,
        handle.doc(),
        "cmd-steer-park-timer",
        SessionCommandPayload::Steer {
            attachments: Vec::new(),
            input_items: Vec::new(),
            prompt: "next".into(),
            message_id: Some("m-2".into()),
        },
    );
    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Working),
        "turn two working",
    )
    .await;
    let working = core.sessions.session_status(CHAT).unwrap();
    let started = working.started_at.expect("Working carries a timer base");
    assert!(
        started >= before_steer,
        "steer into a parked session must restamp started_at (got {started}, steered at {before_steer})"
    );

    wait_for(
        || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
        "turn two to settle",
    )
    .await;
    assert_eq!(core.sessions.session_status(CHAT).unwrap().started_at, None);
}

#[tokio::test]
async fn context_usage_settles_after_done_without_reopening_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(
        dir.path(),
        Arc::new(ScriptedHarness {
            script: vec![
                AgentEvent::TextDelta {
                    text: "Finished".into(),
                },
                AgentEvent::ContextUsage {
                    tokens: Some(64000),
                    window: Some(200000),
                },
                done(DoneStatus::Completed),
                AgentEvent::Subagent {
                    parent_tool_use_id: "child".into(),
                    event: Box::new(AgentEvent::ContextUsage {
                        tokens: Some(999999),
                        window: Some(1000000),
                    }),
                },
                AgentEvent::ContextUsage {
                    tokens: Some(0),
                    window: None,
                },
            ],
            step_delay: Duration::from_millis(20),
            hang_until_interrupt: false,
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "context-run",
        SessionCommandPayload::Run {
            request: run_request("measure context"),
            message_id: "context-user".into(),
        },
    );
    wait_for(
        || {
            handle
                .doc()
                .context_usage()
                .is_some_and(|u| u.tokens == Some(0))
        },
        "post-turn context update",
    )
    .await;
    assert_eq!(
        handle.doc().context_usage(),
        Some(luna_proto::ContextUsage {
            tokens: Some(0),
            window: Some(200000)
        })
    );
    assert_eq!(
        core.sessions.session_status(CHAT).map(|s| s.status),
        Some(SessionStatus::Idle)
    );
    assert_eq!(
        entries(&core).len(),
        2,
        "usage does not create transcript rows"
    );
}

/// Reproduce a steer accepted just before the old turn's Done. The harness
/// confirms the new boundary later; only its eventual completion may notify.
#[tokio::test]
async fn pending_steer_handoff_does_not_publish_a_completion() {
    struct ControlledHarness(
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<AgentEvent>>>,
    );
    #[async_trait]
    impl Harness for ControlledHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Controlled"
        }
        fn supports_steering(&self) -> bool {
            true
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::StepBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[ReasoningLevel::Medium]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let rx = self.0.lock().unwrap().take().unwrap();
            // Acknowledge mailbox delivery, while the test controls the
            // distinct transcript boundary and terminal provider events.
            Ok(
                futures::stream::unfold((rx, controls), |(mut rx, mut controls)| async move {
                    loop {
                        tokio::select! {
                            event = rx.recv() => return event.map(|event| (Ok(event), (rx, controls))),
                            Some(message) = controls.steering.recv() => {
                                if let Some(acceptance) = message.acceptance {
                                    let _ = acceptance.send(SteerAcceptance::Accepted);
                                }
                            }
                        }
                    }
                })
                .boxed(),
            )
        }
    }
    for routed_dispatch in [false, true] {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let core = assemble(
            dir.path(),
            Arc::new(ControlledHarness(std::sync::Mutex::new(Some(rx)))),
        );
        let handle = core.doc_host.open(CHAT).unwrap();
        queue_as_owner(
            &core,
            handle.doc(),
            "cmd-completion",
            SessionCommandPayload::Run {
                request: run_request("opening"),
                message_id: "user-opening".into(),
            },
        );
        tx.send(mock_script()[0].clone()).unwrap();
        wait_for(
            || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Working),
            "opening run",
        )
        .await;
        if routed_dispatch {
            core.sessions
                .dispatch(
                    CHAT,
                    HarnessId::Mock,
                    run_request("redirect"),
                    Some("user-steer".into()),
                )
                .await
                .unwrap();
        } else {
            assert_eq!(
                core.sessions
                    .steer(CHAT, "redirect", Some("user-steer".into()))
                    .await
                    .unwrap(),
                luna_engine::sessions::SteerOutcome::Accepted
            );
        }
        tx.send(done(DoneStatus::Completed)).unwrap();
        wait_for(
            || core.sessions.session_status(CHAT).map(|s| s.status) == Some(SessionStatus::Idle),
            "internal handoff",
        )
        .await;
        let completion = core
            .sessions
            .session_status(CHAT)
            .unwrap()
            .last_completed_turn;
        assert_eq!(completion.is_some(), !routed_dispatch);
        if !routed_dispatch {
            // An explicit steer without an incorporation boundary is retired
            // as unknown at Done; it must never create a follow-up turn.
            assert_eq!(
                entries(&core)
                    .iter()
                    .filter(|entry| entry.role == MessageRole::User)
                    .count(),
                1
            );
            drop(tx);
            core.shutdown().await;
            continue;
        }
        tx.send(AgentEvent::Steered {
            user_message_id: None,
            assistant_message_id: Some("a-1".into()),
            next_assistant_message_id: Some("a-steered".into()),
        })
        .unwrap();
        tx.send(AgentEvent::TextDelta {
            text: "redirected response".into(),
        })
        .unwrap();
        tx.send(done(DoneStatus::Completed)).unwrap();
        wait_for(
            || {
                core.sessions
                    .session_status(CHAT)
                    .and_then(|s| s.last_completed_turn)
                    .as_deref()
                    == Some("a-steered")
            },
            "real completion",
        )
        .await;
        // A duplicate terminal frame from a parked runtime cannot ring twice.
        let (_, mut events) = core.sessions.subscribe(CHAT, 0).unwrap();
        tx.send(done(DoneStatus::Completed)).unwrap();
        let duplicate = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(duplicate.event, AgentEvent::Done { .. }));
        // drive_run publishes and settles synchronously before polling again.
        tokio::task::yield_now().await;
        drop(tx);
        assert_eq!(
            core.sessions
                .session_status(CHAT)
                .unwrap()
                .last_completed_turn
                .as_deref(),
            Some("a-steered")
        );
        core.shutdown().await;
    }
}

/// Real drive_run + journal + Loro, with an isolated Codex source root.
#[tokio::test]
async fn generated_image_is_materialized_before_publication_and_survives_reopen() {
    use luna_engine::{DocHost, DocHostConfig, SessionsEngine, Uploads};
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("codex/generated_images");
    std::fs::create_dir_all(&source_root).unwrap();
    let source = source_root.join("source.png");
    let bytes = b"\x89PNG\r\n\x1a\nBASE64_SENTINEL_ONLY_IN_FILE";
    std::fs::write(&source, bytes).unwrap();
    let image = AgentEvent::GeneratedImage {
        id: "image-1:image".into(),
        path: source.to_string_lossy().into_owned(),
        name: "untrusted".into(),
        mime_type: "untrusted".into(),
    };
    let script = vec![
        AgentEvent::ToolCall {
            id: "image-1".into(),
            call: ToolCall::Unknown {
                name: "Generate image".into(),
                input: None,
            },
        },
        AgentEvent::ToolResult {
            id: "image-1".into(),
            is_error: false,
            output: None,
            diff: None,
        },
        image.clone(),
        image.clone(),
        done(DoneStatus::Completed),
    ];
    let registry = registry_with(Arc::new(MockHarness { script }));
    let journal = Arc::new(RunJournal::open(dir.path().join("journals")).unwrap());
    let store = Arc::new(DocsStore::open(dir.path()).unwrap());
    let sessions = SessionsEngine::new("host".into(), journal.clone(), registry, store.clone());
    let uploads = Uploads::from_root(&dir.path().join("profile/uploads"));
    sessions.set_generated_images(uploads.clone(), source_root);
    let host = DocHost::new(
        store,
        DocHostConfig {
            device_id: "host".into(),
            default_harness: HarnessId::Mock,
            edge: None,
        },
    );
    sessions.set_doc_host(host.clone());
    sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("image"), None)
        .await
        .unwrap();
    wait_for(
        || {
            sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "image completion",
    )
    .await;
    let handle = host.open(CHAT).unwrap();
    let entries = handle.doc().read_entries().unwrap();
    let images: Vec<_> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| {
            if let MessagePart::Image { path, .. } = p {
                Some(path.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(images.len(), 1);
    assert!(std::path::Path::new(&images[0]).starts_with(uploads.dir().canonicalize().unwrap()));
    let serialized = serde_json::to_string(&journal.replay(CHAT, 0).unwrap()).unwrap();
    assert!(!serialized.contains(source.to_str().unwrap()));
    assert!(!serialized.contains("BASE64_SENTINEL"));
    let doc_json = serde_json::to_string(&entries).unwrap();
    assert!(!doc_json.contains(source.to_str().unwrap()));
    assert!(!doc_json.contains("BASE64_SENTINEL"));
    std::fs::remove_file(&source).unwrap();
    assert_eq!(std::fs::read(&images[0]).unwrap(), bytes);
    let imported = loro::LoroDoc::new();
    imported
        .import(&handle.doc().export_snapshot().unwrap())
        .unwrap();
    assert_eq!(
        SessionDoc::from_doc(imported).read_entries().unwrap(),
        entries
    );
    // Resume echoes the successful completed item after Codex removed its source.
    sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("resume"), None)
        .await
        .unwrap();
    wait_for(
        || {
            sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "resume completion",
    )
    .await;
    assert_eq!(
        handle
            .doc()
            .read_entries()
            .unwrap()
            .iter()
            .flat_map(|e| &e.parts)
            .filter(|p| matches!(p, MessagePart::Image { .. }))
            .count(),
        1
    );
    assert!(
        !handle
            .doc()
            .read_entries()
            .unwrap()
            .iter()
            .flat_map(|e| &e.parts)
            .any(|p| matches!(p, MessagePart::Error { .. }))
    );
    sessions.shutdown().await;
}

/// Real provider + engine smoke, opt-in because it consumes image quota.
#[tokio::test]
#[ignore = "requires authenticated Codex with image generation; consumes quota"]
async fn real_image_generation_profile_smoke() {
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        registry_with(Arc::new(luna_harness::CodexHarness::new())),
        HarnessId::Codex,
        None,
    )
    .unwrap();
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::Codex,
            run_request("Generate an image of a small green goblin using image generation."),
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(300), async {
        while !entries_now(&core)
            .iter()
            .any(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap();
    let all = entries(&core);
    let image = all
        .iter()
        .flat_map(|e| &e.parts)
        .find_map(|p| {
            if let MessagePart::Image { path, .. } = p {
                Some(path)
            } else {
                None
            }
        })
        .expect("generated image reaches document");
    assert!(std::path::Path::new(image).starts_with(core.uploads.dir()));
    assert!(std::path::Path::new(image).is_file());
    assert!(
        !serde_json::to_string(&all)
            .unwrap()
            .contains("generated_images/")
    );
    core.sessions.shutdown().await;
}

/// Synthetic harness → engine pending map → journal/document → owner commands
/// and live URL RPC. Native UI/provider evidence is a separate coordinator step.
#[tokio::test]
async fn mcp_form_url_validation_concurrency_and_secret_boundaries_e2e() {
    use luna_proto::{McpFieldSchema, McpInput, McpInputKind, UserInputAnswer, UserInputQuestion};
    use std::sync::Mutex;
    struct McpHarness {
        observed: Arc<Mutex<Vec<serde_json::Value>>>,
    }
    #[async_trait]
    impl Harness for McpHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "MCP fixture"
        }
        fn supports_steering(&self) -> bool {
            false
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::TurnBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let observed = self.observed.clone();
            let stop_scenario = request.prompt.contains("stop");
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                let expires_at_ms = chrono::Utc::now().timestamp_millis() + 120_000;
                let binding = |id: &str, kind| McpInput {
                    server_name: "synthetic-server".into(),
                    provider_request_id: id.into(),
                    thread_id: "synthetic-native-thread".into(),
                    turn_id: "synthetic-native-turn".into(),
                    expires_at_ms,
                    kind,
                    launch_url: None,
                };
                let schema = McpFieldSchema::parse(
                    &serde_json::json!({"type":"integer", "minimum":0, "maximum":4}),
                    true,
                )
                .unwrap();
                let form = vec![
                    UserInputQuestion {
                        id: "field".into(),
                        header: "Count".into(),
                        question: "Count".into(),
                        options: vec![],
                        multi_select: false,
                        approval: None,
                        mcp: Some(binding(
                            "form",
                            McpInputKind::Field {
                                name: "count".into(),
                                schema: Box::new(schema),
                            },
                        )),
                        input: None,
                    },
                    UserInputQuestion {
                        id: "form-action".into(),
                        header: "MCP".into(),
                        question: "Submit?".into(),
                        options: vec!["Accept".into(), "Decline".into(), "Cancel".into()],
                        multi_select: false,
                        approval: None,
                        mcp: Some(binding("form", McpInputKind::FormAction)),
                        input: None,
                    },
                ];
                let mut url_meta = binding(
                    "url",
                    McpInputKind::Url {
                        destination: "https://example.invalid".into(),
                        elicitation_id: "synthetic-flow".into(),
                    },
                );
                url_meta.launch_url = Some(
                    "https://example.invalid/auth?token=private-url-token#fragment-secret".into(),
                );
                let url = vec![UserInputQuestion {
                    id: "url-action".into(),
                    header: "MCP URL".into(),
                    question: "Open example.invalid".into(),
                    options: vec!["Accept".into(), "Decline".into(), "Cancel".into()],
                    multi_select: false,
                    approval: None,
                    mcp: Some(url_meta),
                    input: None,
                }];
                assert!(!format!("{url:?}").contains("private-url-token"));
                let form_rx = (controls.request_input)(form.clone());
                let url_rx = (controls.request_input)(url.clone());
                if stop_scenario {
                    controls.interrupt.cancelled().await;
                    drop(form_rx);
                    drop(url_rx);
                    let _ = tx.send(Ok(done(DoneStatus::Interrupted))).await;
                    return;
                }
                let answers = form_rx.await.unwrap();
                observed
                    .lock()
                    .unwrap()
                    .push(luna_proto::mcp_response(&form, &answers).unwrap());
                let answers = url_rx.await.unwrap();
                observed
                    .lock()
                    .unwrap()
                    .push(luna_proto::mcp_response(&url, &answers).unwrap());
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let core = assemble(
        dir.path(),
        Arc::new(McpHarness {
            observed: observed.clone(),
        }),
    );
    let handle = core.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &core,
        handle.doc(),
        "mcp-run",
        SessionCommandPayload::Run {
            request: run_request("MCP fixture"),
            message_id: "mcp-user".into(),
        },
    );
    let pending = || {
        entries_now(&core)
            .iter()
            .flat_map(|e| &e.parts)
            .filter_map(|part| match part {
                MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } => Some((request_id.clone(), questions.clone())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    wait_for(|| pending().len() == 2, "two concurrent MCP prompts").await;
    let prompts = pending();
    let form_id = prompts
        .iter()
        .find(|(_, q)| q.len() == 2)
        .unwrap()
        .0
        .clone();
    let url_id = prompts
        .iter()
        .find(|(_, q)| q.len() == 1)
        .unwrap()
        .0
        .clone();
    let visible = serde_json::to_string(&prompts).unwrap();
    assert!(!visible.contains("private-url-token") && !visible.contains("fragment-secret"));
    assert!(
        prompts
            .iter()
            .flat_map(|(_, q)| q)
            .all(|q| q.mcp.as_ref().unwrap().launch_url.is_none())
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    let url_params = serde_json::json!({"chatId":CHAT, "requestId":url_id});
    // Repeated read-only fetches do not answer or replace the resolver.
    for _ in 0..2 {
        let result = client
            .call(luna_rpc::methods::MCP_ELICITATION_URL, url_params.clone())
            .await
            .unwrap();
        assert_eq!(
            result["url"],
            "https://example.invalid/auth?token=private-url-token#fragment-secret"
        );
    }
    assert!(observed.lock().unwrap().is_empty());
    for params in [
        serde_json::json!({"chatId":"foreign", "requestId":url_id}),
        serde_json::json!({"chatId":CHAT, "requestId":"foreign"}),
        serde_json::json!({"chatId":CHAT, "requestId":form_id}),
    ] {
        let error = client
            .call(luna_rpc::methods::MCP_ELICITATION_URL, params)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("private-url-token"));
    }
    let form_answers = |value: &str| {
        vec![
            UserInputAnswer {
                question_id: "field".into(),
                labels: vec![value.into()],
            },
            UserInputAnswer {
                question_id: "form-action".into(),
                labels: vec!["Accept".into()],
            },
        ]
    };
    queue_as_owner(
        &core,
        handle.doc(),
        "mcp-invalid",
        SessionCommandPayload::RespondInput {
            request_id: form_id.clone(),
            answers: form_answers("99"),
        },
    );
    wait_for(
        || {
            command_status(&core, "mcp-invalid")
                .is_some_and(|(s, _)| s == SessionCommandStatus::Rejected)
        },
        "invalid MCP answer rejection",
    )
    .await;
    assert_eq!(pending().len(), 2);
    queue_as_owner(
        &core,
        handle.doc(),
        "mcp-valid",
        SessionCommandPayload::RespondInput {
            request_id: form_id,
            answers: form_answers("0"),
        },
    );
    wait_for(
        || observed.lock().unwrap().len() == 1 && pending().len() == 1,
        "first prompt resolves independently",
    )
    .await;
    assert_eq!(
        observed.lock().unwrap()[0],
        serde_json::json!({"action":"accept", "content":{"count":0}})
    );
    assert_eq!(
        core.sessions.session_status(CHAT).unwrap().status,
        SessionStatus::AwaitingInput
    );
    queue_as_owner(
        &core,
        handle.doc(),
        "mcp-cancel",
        SessionCommandPayload::RespondInput {
            request_id: url_id,
            answers: vec![UserInputAnswer {
                question_id: "url-action".into(),
                labels: vec!["Cancel".into()],
            }],
        },
    );
    wait_for(
        || observed.lock().unwrap().len() == 2 && pending().is_empty(),
        "second prompt cancels independently",
    )
    .await;
    assert_eq!(
        observed.lock().unwrap()[1],
        serde_json::json!({"action":"cancel"})
    );
    assert!(
        client
            .call(luna_rpc::methods::MCP_ELICITATION_URL, url_params)
            .await
            .is_err()
    );
    let journal_path = dir
        .path()
        .join("orgs/dev-org/dev-user/journals")
        .join(format!("{CHAT}.jsonl"));
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    assert!(!journal.contains("private-url-token") && !journal.contains("fragment-secret"));
    if let Some(output) = std::env::var_os("LUNA_MCP_E2E_ARTIFACT_DIR") {
        let output = std::path::PathBuf::from(output);
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("engine-form-url.json"), serde_json::to_vec_pretty(&serde_json::json!({"synthetic":true,"responses":*observed.lock().unwrap(),"prompts":prompts,"journal":journal,"checks":["concurrent","invalid-answer-retained","exact-request","repeat-url-read","raw-url-not-durable","cancel"]})).unwrap()).unwrap();
    }
    // Stop then reopen a real engine data root with unanswered MCP prompts.
    // A stale Accept must not dispatch the orphan fallback as another run.
    wait_for(
        || {
            core.sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "initial MCP run settles",
    )
    .await;
    queue_as_owner(
        &core,
        handle.doc(),
        "mcp-stop-run",
        SessionCommandPayload::Run {
            request: run_request("stop MCP fixture"),
            message_id: "mcp-stop-user".into(),
        },
    );
    wait_for(|| pending().len() == 2, "MCP prompts before stop").await;
    let stale_prompts = pending();
    let crash_snapshot_entries = entries(&core);
    let stale_form_id = stale_prompts
        .iter()
        .find(|(_, q)| q.len() == 2)
        .unwrap()
        .0
        .clone();
    let stale_url_id = stale_prompts
        .iter()
        .find(|(_, q)| q.len() == 1)
        .unwrap()
        .0
        .clone();
    core.sessions.interrupt_for_stop(CHAT).await.unwrap();
    wait_for(|| !core.sessions.has_live_run(CHAT), "stopped MCP run").await;
    assert!(
        client
            .call(
                luna_rpc::methods::MCP_ELICITATION_URL,
                serde_json::json!({"chatId":CHAT,"requestId":stale_url_id})
            )
            .await
            .is_err()
    );
    core.shutdown().await;
    drop(client);
    drop(handle);
    drop(core);
    // Model the last durable snapshot of an abrupt disconnect. A graceful
    // stop resolves its inputs, so retain the pre-stop input projection in a
    // settled synthetic snapshot to exercise the actual orphan recovery path.
    let crash_doc = SessionDoc::init(CHAT).unwrap();
    for mut entry in crash_snapshot_entries {
        if entry.status == Some(MessageStatus::Streaming) {
            entry.status = Some(MessageStatus::Aborted);
        }
        crash_doc.push_message(&entry).unwrap();
    }
    let store = DocsStore::open(dir.path().join("orgs/dev-org/dev-user")).unwrap();
    store
        .save_snapshot(CHAT, &crash_doc.export_snapshot().unwrap())
        .unwrap();
    drop(store);
    let restarted = assemble(
        dir.path(),
        Arc::new(McpHarness {
            observed: observed.clone(),
        }),
    );
    let recovered = restarted.doc_host.open(CHAT).unwrap();
    queue_as_owner(
        &restarted,
        recovered.doc(),
        "mcp-stale-accept",
        SessionCommandPayload::RespondInput {
            request_id: stale_form_id.clone(),
            answers: form_answers("0"),
        },
    );
    wait_for(
        || {
            command_status(&restarted, "mcp-stale-accept")
                .is_some_and(|(s, _)| s == SessionCommandStatus::Rejected)
        },
        "stale MCP acceptance rejected after restart",
    )
    .await;
    assert!(!restarted.sessions.has_live_run(CHAT));
    assert_eq!(observed.lock().unwrap().len(), 2);
    let restart_client = luna_rpc::memory_client(restarted.rpc_service());
    let error = restart_client
        .call(
            luna_rpc::methods::MCP_ELICITATION_URL,
            serde_json::json!({"chatId":CHAT,"requestId":stale_url_id}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("private-url-token"));
    queue_as_owner(
        &restarted,
        recovered.doc(),
        "mcp-stale-decline",
        SessionCommandPayload::RespondInput {
            request_id: stale_form_id,
            answers: vec![UserInputAnswer {
                question_id: "form-action".into(),
                labels: vec!["Decline".into()],
            }],
        },
    );
    wait_for(
        || {
            command_status(&restarted, "mcp-stale-decline")
                .is_some_and(|(s, _)| s == SessionCommandStatus::Applied)
        },
        "stale prompt closes without provider replay",
    )
    .await;
    if let Some(output) = std::env::var_os("LUNA_MCP_E2E_ARTIFACT_DIR") {
        std::fs::write(std::path::PathBuf::from(output).join("engine-stop-restart.json"), serde_json::to_vec_pretty(&serde_json::json!({"synthetic":true,"checks":["stop-invalidates-url","restart-invalidates-url","stale-accept-rejected","no-orphan-model-turn","stale-decline-local-only"],"entries":entries(&restarted),"error":error})).unwrap()).unwrap();
    }
    restarted.shutdown().await;
}

/// codex-adapter fix 5 (failure cases in evidence/codex-adapter/failure-cases.md):
/// a Codex `error {willRetry:true}` and a `warning` reach the engine through
/// the real adapter as a transient task notice (doc metadata), are cleared
/// when the turn ends, and never become transcript Error parts. A
/// non-retrying error still does.
#[tokio::test]
async fn codex_retry_and_warning_are_transient_notices_not_error_parts() {
    let dir = tempfile::tempdir().unwrap();
    let peer = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/codex-adapter-peer.py");
    let accounts = luna_engine::AgentAccountsConfig {
        data_dir: dir.path().join("synthetic-provider-state"),
        claude_config_dir: dir.path().join("synthetic-claude"),
        claude_config_file: dir.path().join("synthetic-claude.json"),
        codex_home: dir.path().join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        luna_engine::EngineProfile::development(
            &dir.path().join("engine"),
            "synthetic-org",
            "synthetic-user",
        ),
        registry_with(Arc::new(
            luna_harness::CodexHarness::new().with_executable(peer),
        )),
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap();
    let handle = core.doc_host.open(CHAT).unwrap();
    let mut request = run_request("notices");
    request.harness = Some(HarnessId::Codex);
    queue_as_owner(
        &core,
        handle.doc(),
        "notice-run",
        SessionCommandPayload::Run {
            request,
            message_id: "notice-user".into(),
        },
    );
    wait_for(
        || {
            handle.doc().provider_notice().is_some_and(|notice| {
                notice.kind == luna_proto::ProviderNoticeKind::Retrying
                    && notice.message == "Reconnecting... 1/5"
            })
        },
        "retry notice on task metadata",
    )
    .await;
    wait_for(
        || {
            entries_now(&core).iter().any(|entry| {
                entry.role == MessageRole::Assistant
                    && entry
                        .parts
                        .iter()
                        .any(|part| matches!(part, MessagePart::Text { text, .. } if text.contains("peer-verified")))
                    && entry.status == Some(MessageStatus::Complete)
            })
        },
        "completed Codex turn",
    )
    .await;
    wait_for(
        || handle.doc().provider_notice().is_none(),
        "notice cleared at turn end",
    )
    .await;
    let errors: Vec<_> = entries(&core)
        .iter()
        .flat_map(|entry| entry.parts.clone())
        .filter_map(|part| match part {
            MessagePart::Error { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(errors, ["quota exhausted"], "retries are not errors");
    core.shutdown().await;
}

fn save_input_audit_evidence(name: &str, value: &serde_json::Value) {
    let root = std::env::var_os("LUNA_AUDIT_EVIDENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("luna-engine-input-audit-{}", std::process::id()))
        });
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    eprintln!("input audit evidence: {}", path.display());
}

/// A nonblocking question is still real after the root turn settles. Its
/// durable card must survive both parking and a repeated provider event.
#[tokio::test]
async fn parked_async_question_is_visible_once_without_reopening_turn() {
    let dir = tempfile::tempdir().unwrap();
    let question = AgentEvent::AsyncQuestions {
        message_id: "late-nonblocking-call".into(),
        questions: vec![luna_proto::AsyncUserInputQuestion {
            title: "Which background result should I keep?".into(),
            options: Some(vec!["First (Recommended)".into(), "Second".into()]),
            ..Default::default()
        }],
    };
    let core = assemble(
        dir.path(),
        Arc::new(ScriptedHarness {
            script: vec![
                AgentEvent::TextDelta {
                    text: "Root finished".into(),
                },
                done(DoneStatus::Completed),
                question.clone(),
                question,
            ],
            step_delay: Duration::from_millis(25),
            hang_until_interrupt: false,
        }),
    );
    core.sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("start"), None)
        .await
        .unwrap();
    wait_for(|| !core.sessions.has_live_run(CHAT), "parked stream closed").await;
    let entries = entries_now(&core);
    save_input_audit_evidence(
        "parked-async-question",
        &serde_json::json!({
            "entries": entries,
            "session": core.sessions.session_status(CHAT),
        }),
    );
    let cards: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry.parts.iter().any(|part| {
                matches!(part, MessagePart::AsyncQuestion { id, resolved: false, .. }
            if id == "aq-late-nonblocking-call")
            })
        })
        .collect();
    assert_eq!(
        cards.len(),
        1,
        "the accepted late question has one durable card"
    );
    assert_eq!(cards[0].status, Some(MessageStatus::Complete));
    assert_eq!(
        core.sessions.session_status(CHAT).unwrap().status,
        SessionStatus::Idle
    );
    core.shutdown().await;
}

/// Carrying a child question to a new card must keep the original automatic
/// answer deadline and the provider's original one-shot answer channel.
#[tokio::test(flavor = "multi_thread")]
async fn carried_question_preserves_unattended_deadline_and_hold() {
    struct CarriedQuestionHarness {
        root_done: Arc<tokio::sync::Notify>,
        observed: Arc<std::sync::Mutex<Option<Vec<luna_proto::UserInputAnswer>>>>,
    }
    #[async_trait]
    impl Harness for CarriedQuestionHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Mock
        }
        fn display_name(&self) -> &str {
            "Carried question"
        }
        fn supports_steering(&self) -> bool {
            true
        }
        fn steering_mode(&self) -> SteeringMode {
            SteeringMode::StepBoundary
        }
        fn reasoning_levels(&self) -> &[ReasoningLevel] {
            &[]
        }
        async fn models(&self) -> Result<Vec<Model>, HarnessError> {
            Ok(vec![])
        }
        async fn run(
            &self,
            _request: RunRequest,
            controls: RunControls,
        ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let root_done = self.root_done.clone();
            let observed = self.observed.clone();
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(AgentEvent::NativeTaskChanged {
                        task: Box::new(luna_proto::NativeTaskSnapshot {
                            task_id: "asking-child".into(),
                            ..Default::default()
                        }),
                    }))
                    .await;
                let answer = (controls.request_input)(vec![luna_proto::UserInputQuestion {
                    id: "choice".into(),
                    header: "Choose".into(),
                    question: "Which path?".into(),
                    options: vec!["First (Recommended)".into(), "Second".into()],
                    multi_select: false,
                    approval: None,
                    mcp: None,
                    input: None,
                }]);
                root_done.notified().await;
                let _ = tx.send(Ok(done(DoneStatus::Completed))).await;
                let answer = answer.await.unwrap_or_default();
                *observed.lock().unwrap() = Some(answer);
                controls.interrupt.cancelled().await;
            });
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })
            .boxed())
        }
    }
    for hold in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root_done = Arc::new(tokio::sync::Notify::new());
        let observed = Arc::new(std::sync::Mutex::new(None));
        let core = assemble(
            dir.path(),
            Arc::new(CarriedQuestionHarness {
                root_done: root_done.clone(),
                observed: observed.clone(),
            }),
        );
        let settings = dir.path().join("input-settings.json");
        std::fs::write(&settings, br#"{"unattendedAnswerSeconds":2}"#).unwrap();
        core.sessions.set_recovery_settings_path(settings);
        core.sessions
            .dispatch(CHAT, HarnessId::Mock, run_request("start"), None)
            .await
            .unwrap();
        let open_card = || {
            entries_now(&core).into_iter().rev().find_map(|entry| {
                entry.parts.into_iter().find_map(|part| match part {
                    MessagePart::Input {
                        request_id,
                        questions,
                        resolved: false,
                        ..
                    } => Some((request_id, questions)),
                    _ => None,
                })
            })
        };
        wait_for(|| open_card().is_some(), "initial recommended question").await;
        let (old_id, old_questions) = open_card().unwrap();
        let original_deadline = old_questions[0]
            .input
            .as_ref()
            .unwrap()
            .auto_select
            .as_ref()
            .unwrap()
            .at_ms;
        if hold {
            assert!(core.sessions.hold_input(CHAT, &old_id));
        }
        root_done.notify_one();
        wait_for(
            || open_card().is_some_and(|(id, _)| id != old_id),
            "carried question card",
        )
        .await;
        let (new_id, questions) = open_card().unwrap();
        let plan = questions[0]
            .input
            .as_ref()
            .and_then(|input| input.auto_select.as_ref());
        if hold {
            assert!(plan.is_none(), "Hold survives card replacement");
            let remaining = original_deadline
                .saturating_sub(chrono::Utc::now().timestamp_millis())
                .max(0);
            tokio::time::sleep(Duration::from_millis(remaining as u64 + 100)).await;
            assert!(
                observed.lock().unwrap().is_none(),
                "held request is never auto-answered"
            );
            assert!(
                core.sessions
                    .respond_input(
                        CHAT,
                        &new_id,
                        vec![luna_proto::UserInputAnswer {
                            question_id: "choice".into(),
                            labels: vec!["Second".into()],
                        }]
                    )
                    .unwrap()
            );
        } else {
            assert_eq!(
                plan.map(|plan| plan.at_ms),
                Some(original_deadline),
                "carrying a question preserves, rather than restarts, its countdown"
            );
        }
        wait_for(
            || observed.lock().unwrap().is_some(),
            "native child receives its answer",
        )
        .await;
        wait_for(|| open_card().is_none(), "carried card resolved").await;
        let answers = observed.lock().unwrap().clone().unwrap();
        save_input_audit_evidence(
            if hold {
                "carried-question-held"
            } else {
                "carried-question-auto"
            },
            &serde_json::json!({"oldRequestId": old_id, "newRequestId": new_id,
                "deadline": original_deadline, "answers": answers, "entries": entries_now(&core)}),
        );
        assert_eq!(
            answers[0].labels,
            [if hold {
                "Second"
            } else {
                "First (Recommended)"
            }]
        );
        core.sessions.interrupt(CHAT).await.unwrap();
        core.shutdown().await;
    }
}
