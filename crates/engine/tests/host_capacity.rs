//! AW-10 host turn capacity through the production task-command, queue,
//! recovery and session boundaries, on a disposable profile with a synthetic
//! provider whose turns the test releases:
//! - a full host holds new turns with a reason and a place in line, and a
//!   held send is withdrawn unsent (a started one is never "cancelled");
//! - held sends survive a restart without becoming "delivery unknown" and
//!   are each delivered exactly once;
//! - a due automatic retry waits for a slot unsent, and its cancellation
//!   survives restart;
//! - a queued follow-up waits unclaimed.
//!
//! Evidence JSON lands in `$LUNA_AW10_EVIDENCE_DIR` when it is set.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, HostCapacity,
    HostCapacityWaitKind, Model, PermissionMode, ReasoningLevel, RecoveryStatus, RunRequest,
    SandboxLevel, SteeringMode, TaskCommandRead, TaskRecovery,
};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};

/// The synthetic provider's turns: every prompt it received, and a release
/// for each held turn. A prompt containing "overload" is rejected before
/// acceptance the first time it is seen (a retryable provider overload).
#[derive(Clone, Default)]
struct Turns {
    prompts: Arc<Mutex<Vec<String>>>,
    releases: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
}

impl Turns {
    /// How many provider turns started with a prompt containing `word`.
    fn count(&self, word: &str) -> usize {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|prompt| prompt.contains(word))
            .count()
    }

    /// End the held turn whose prompt contains `word`.
    fn release(&self, word: &str) {
        let mut releases = self.releases.lock().unwrap();
        let key = releases
            .keys()
            .find(|prompt| prompt.contains(word))
            .cloned()
            .unwrap_or_else(|| panic!("no held turn for {word}"));
        let _ = releases.remove(&key).unwrap().send(());
    }
}

struct CapacityHarness(Turns);

#[async_trait]
impl Harness for CapacityHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "AW-10 synthetic"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (run, first) = {
            let mut prompts = self.0.prompts.lock().unwrap();
            let first = !prompts.contains(&request.prompt);
            prompts.push(request.prompt.clone());
            (prompts.len(), first)
        };
        if first && request.prompt.contains("overload") {
            return Err(HarnessError::RejectedBeforeAcceptance(
                "synthetic provider overloaded".into(),
            ));
        }
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        self.0
            .releases
            .lock()
            .unwrap()
            .insert(request.prompt.clone(), release);
        let session_id = format!("aw10-session-{run}");
        let started = futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: request.model.clone().unwrap_or_else(|| "mock-1".into()),
                tools: Vec::new(),
                cwd: request.cwd.clone(),
                session_id: session_id.clone(),
                assistant_message_id: format!("aw10-assistant-{run}"),
            }),
            Ok(AgentEvent::TextDelta {
                text: "holding a turn slot".into(),
            }),
        ]);
        let finished = futures::stream::once(async move {
            let _ = released.await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session_id),
            })
        });
        Ok(started.chain(finished).boxed())
    }
}

fn assemble(root: &Path, turns: &Turns) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CapacityHarness(turns.clone())));
    EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&root.join("data"), "aw10-org", "aw10-user"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
        AgentAccountsConfig {
            data_dir: root.join("provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
        },
    )
    .unwrap()
}

fn create_tasks(core: &EngineCore, ids: &[&str]) {
    for id in ids {
        core.workspace
            .create_chat(id, None, Some(&core.device_id), None, Some("/tmp".into()))
            .unwrap();
    }
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: Some("mock-1".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: Some(PermissionMode::Ask),
        environment_permission_ceiling: Some(PermissionMode::Ask),
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

/// One task command with the fences the task's current snapshot requires.
async fn command(client: &RpcClient, task: &str, id: &str, intent: Value) -> CommandReceipt {
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({ "taskId": task }),
        )
        .await
        .unwrap();
    let mut value = json!({
        "commandId": id,
        "actorId": "aw10-test",
        "clientId": "aw10-test",
        "clientSessionId": "isolated",
        "taskId": task,
        "intent": intent,
    });
    if let Some(generation) = read.snapshot.binding_generation {
        value["expectedBindingGeneration"] = generation.into();
    }
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, value)
        .await
        .unwrap()
}

/// A new message: the Run command the composer sends to an idle task.
async fn send(client: &RpcClient, task: &str, id: &str, prompt: &str) -> CommandReceipt {
    command(
        client,
        task,
        id,
        json!({
            "kind": "session",
            "command": {
                "kind": "run",
                "request": request(prompt),
                "messageId": format!("{id}-message"),
            },
            "transfers": [],
        }),
    )
    .await
}

async fn read_capacity(client: &RpcClient) -> HostCapacity {
    client
        .call_as(methods::GET_HOST_CAPACITY, json!({}))
        .await
        .unwrap()
}

async fn set_limit(client: &RpcClient, limit: Option<u32>) -> HostCapacity {
    client
        .call_as(methods::SET_HOST_CAPACITY, json!({ "limit": limit }))
        .await
        .unwrap()
}

fn receipt(core: &EngineCore, command_id: &str) -> CommandReceipt {
    core.doc_host
        .task_command_receipt(command_id)
        .unwrap()
        .unwrap_or_else(|| panic!("no receipt for {command_id}"))
}

fn transcript_has(core: &EngineCore, task: &str, message_id: &str) -> bool {
    core.doc_host
        .open(task)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .iter()
        .any(|entry| entry.id == message_id)
}

fn recovery(core: &EngineCore, task: &str) -> Option<TaskRecovery> {
    core.sessions
        .session_status(task)
        .and_then(|session| session.recovery)
}

/// Poll an engine readback until it answers, within a bounded deadline.
async fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Longer than several capacity re-checks (one per second): long enough for
/// a start that should stay held to have been admitted if it were not.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(2_500)).await;
}

fn line(capacity: &HostCapacity) -> Vec<(String, HostCapacityWaitKind, String, u32)> {
    capacity
        .waiting
        .iter()
        .map(|wait| {
            (
                wait.task_id.clone(),
                wait.kind,
                wait.source_id.clone(),
                wait.position,
            )
        })
        .collect()
}

fn s(text: &str) -> String {
    text.to_owned()
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("LUNA_AW10_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

/// C1, C2, C4, C5, C6, C10, C12: with the host full, new sends wait in issue
/// order with nothing written or sent; Cancel withdraws one unsent; lowering
/// the limit stops nothing; a freed slot admits the next in line; a send that
/// started is never reported cancelled.
#[tokio::test]
async fn full_host_holds_new_turns_with_a_reason_and_cancel_withdraws_them_unsent() {
    let root = tempfile::tempdir().unwrap();
    let turns = Turns::default();
    let core = assemble(root.path(), &turns);
    create_tasks(&core, &["aw10-a", "aw10-b", "aw10-c", "aw10-d"]);
    let client = luna_rpc::memory_client(core.rpc_service());

    for limit in [0, luna_proto::HOST_TURN_LIMIT_MAX + 1] {
        assert!(
            client
                .call(methods::SET_HOST_CAPACITY, json!({ "limit": limit }))
                .await
                .is_err(),
            "limit {limit} must be refused"
        );
    }
    let unlimited = read_capacity(&client).await;
    assert_eq!(unlimited.limit, None, "no limit until one is chosen");
    assert_eq!(set_limit(&client, Some(2)).await.limit, Some(2));

    send(&client, "aw10-a", "aw10-send-a", "hold alpha").await;
    send(&client, "aw10-b", "aw10-send-b", "hold bravo").await;
    eventually("two active turns", || {
        (core.sessions.turn_in_flight("aw10-a") && core.sessions.turn_in_flight("aw10-b"))
            .then_some(())
    })
    .await;

    let held_c = send(&client, "aw10-c", "aw10-send-c", "hold charlie").await;
    assert_eq!(held_c.status, CommandReceiptStatus::Pending);
    send(&client, "aw10-d", "aw10-send-d", "hold delta").await;
    eventually("two held sends", || {
        (core.sessions.host_capacity().waiting.len() == 2).then_some(())
    })
    .await;
    let full = read_capacity(&client).await;
    assert_eq!(full.limit, Some(2));
    assert_eq!(full.occupied(), 2);
    assert_eq!(
        line(&full),
        vec![
            (s("aw10-c"), HostCapacityWaitKind::Send, s("aw10-send-c"), 1),
            (s("aw10-d"), HostCapacityWaitKind::Send, s("aw10-send-d"), 2),
        ],
        "held sends wait in issue order with their reason"
    );
    assert_eq!(turns.count("charlie") + turns.count("delta"), 0);
    assert!(
        !transcript_has(&core, "aw10-c", "aw10-send-c-message"),
        "a held send writes nothing to the transcript"
    );

    let cancel = command(
        &client,
        "aw10-c",
        "aw10-cancel-c",
        json!({ "kind": "cancelCapacityWait", "commandId": "aw10-send-c" }),
    )
    .await;
    assert_eq!(cancel.status, CommandReceiptStatus::Applied, "{cancel:?}");
    let withdrawn = receipt(&core, "aw10-send-c");
    assert_eq!(withdrawn.status, CommandReceiptStatus::Rejected);
    assert!(
        withdrawn
            .reason
            .as_ref()
            .is_some_and(|reason| reason.message.contains("nothing was sent")),
        "{withdrawn:?}"
    );
    let after_cancel = core.sessions.host_capacity();
    assert_eq!(
        line(&after_cancel),
        vec![(s("aw10-d"), HostCapacityWaitKind::Send, s("aw10-send-d"), 1)],
        "the withdrawn send leaves the line at once"
    );
    let again = command(
        &client,
        "aw10-c",
        "aw10-cancel-c-again",
        json!({ "kind": "cancelCapacityWait", "commandId": "aw10-send-c" }),
    )
    .await;
    assert_eq!(again.status, CommandReceiptStatus::Rejected, "{again:?}");

    let lowered = set_limit(&client, Some(1)).await;
    assert_eq!(lowered.limit, Some(1));
    assert_eq!(lowered.active.len(), 2, "lowering the limit stops nothing");
    assert!(core.sessions.turn_in_flight("aw10-a") && core.sessions.turn_in_flight("aw10-b"));
    turns.release("alpha");
    eventually("alpha's turn ends", || {
        (!core.sessions.turn_in_flight("aw10-a")).then_some(())
    })
    .await;
    settle().await;
    let still_full = core.sessions.host_capacity();
    assert_eq!(still_full.occupied(), 1);
    assert_eq!(
        still_full.waiting.len(),
        1,
        "1 of 1 active: delta keeps waiting"
    );
    assert_eq!(turns.count("delta"), 0);

    set_limit(&client, Some(2)).await;
    eventually("delta starts in the freed slot", || {
        core.sessions.turn_in_flight("aw10-d").then_some(())
    })
    .await;
    assert_eq!(turns.count("delta"), 1);
    assert!(core.sessions.host_capacity().waiting.is_empty());
    let late = command(
        &client,
        "aw10-d",
        "aw10-cancel-d",
        json!({ "kind": "cancelCapacityWait", "commandId": "aw10-send-d" }),
    )
    .await;
    assert_eq!(
        late.status,
        CommandReceiptStatus::Rejected,
        "a started send is never reported cancelled"
    );
    assert!(
        core.sessions.turn_in_flight("aw10-d"),
        "the refused cancel leaves the started turn running"
    );
    assert_eq!(turns.count("charlie"), 0, "the withdrawn send never ran");
    let running = read_capacity(&client).await;

    evidence(
        "full-host-line.json",
        json!({
            "unlimited": unlimited,
            "full": full,
            "cancel": cancel,
            "withdrawnSend": withdrawn,
            "secondCancel": again,
            "lowered": lowered,
            "stillFull": still_full,
            "lateCancel": late,
            "running": running,
            "providerPrompts": *turns.prompts.lock().unwrap(),
        }),
    );
    turns.release("bravo");
    turns.release("delta");
    core.shutdown().await;
}

/// C3, C6, C13: held sends stay pending and unprocessed across a restart —
/// never closed as an unknown delivery — and each is delivered exactly once;
/// the limit persists; the interrupted turn is not replayed.
#[tokio::test]
async fn held_sends_survive_restart_and_are_delivered_exactly_once() {
    let root = tempfile::tempdir().unwrap();
    let turns = Turns::default();
    let core = assemble(root.path(), &turns);
    create_tasks(&core, &["aw10-a", "aw10-b", "aw10-c"]);
    let client = luna_rpc::memory_client(core.rpc_service());
    set_limit(&client, Some(1)).await;
    send(&client, "aw10-a", "aw10-send-a", "hold alpha").await;
    eventually("alpha active", || {
        core.sessions.turn_in_flight("aw10-a").then_some(())
    })
    .await;
    send(&client, "aw10-b", "aw10-send-b", "hold bravo").await;
    send(&client, "aw10-c", "aw10-send-c", "hold charlie").await;
    eventually("two held sends", || {
        (core.sessions.host_capacity().waiting.len() == 2).then_some(())
    })
    .await;
    let before = read_capacity(&client).await;
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(root.path(), &turns);
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let restored = read_capacity(&client).await;
    assert_eq!(
        restored.limit,
        Some(1),
        "the limit persists for the profile"
    );
    // Opening a task's document re-evaluates its pending send.
    reopened.doc_host.open("aw10-b").unwrap();
    reopened.doc_host.open("aw10-c").unwrap();
    let (first, second) = eventually("one held send starts after restart", || {
        match (turns.count("bravo"), turns.count("charlie")) {
            (1, 0) => Some((
                ("bravo", "aw10-send-b"),
                ("charlie", "aw10-c", "aw10-send-c"),
            )),
            (0, 1) => Some((
                ("charlie", "aw10-send-c"),
                ("bravo", "aw10-b", "aw10-send-b"),
            )),
            _ => None,
        }
    })
    .await;
    let waiting = eventually("the other send waits again", || {
        let capacity = reopened.sessions.host_capacity();
        (capacity.waiting.len() == 1).then_some(capacity)
    })
    .await;
    assert_eq!(
        line(&waiting),
        vec![(s(second.1), HostCapacityWaitKind::Send, s(second.2), 1)]
    );
    let pending = receipt(&reopened, second.2);
    assert_eq!(pending.status, CommandReceiptStatus::Pending);
    assert!(
        pending.committed_at_ms.is_none(),
        "a held send is never closed as an unknown delivery: {pending:?}"
    );
    assert!(!transcript_has(
        &reopened,
        second.1,
        &format!("{}-message", second.2)
    ));
    eventually("the first send is accepted", || {
        (receipt(&reopened, first.1).status == CommandReceiptStatus::Applied).then_some(())
    })
    .await;

    turns.release(first.0);
    eventually("the second send is accepted", || {
        (receipt(&reopened, second.2).status == CommandReceiptStatus::Applied).then_some(())
    })
    .await;
    settle().await;
    assert_eq!(turns.count("bravo"), 1, "delivered exactly once");
    assert_eq!(turns.count("charlie"), 1, "delivered exactly once");
    assert_eq!(
        turns.count("alpha"),
        1,
        "the interrupted turn is not replayed"
    );
    assert!(reopened.sessions.host_capacity().waiting.is_empty());

    evidence(
        "restart-held-sends.json",
        json!({
            "beforeRestart": before,
            "afterRestart": restored,
            "waitingAgain": waiting,
            "heldReceiptAfterRestart": pending,
            "firstDelivered": receipt(&reopened, first.1),
            "secondDelivered": receipt(&reopened, second.2),
            "providerPrompts": *turns.prompts.lock().unwrap(),
        }),
    );
    turns.release(second.0);
    reopened.shutdown().await;
}

/// C1, C7 (preserved), C12: a due automatic retry finds the host full and
/// stays Scheduled, unsent, with its reason and place in line; Cancel
/// withdraws it, it leaves the line, and the cancellation survives restart
/// without a retry.
#[tokio::test]
async fn automatic_retry_waits_for_a_slot_and_cancel_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let turns = Turns::default();
    let core = assemble(root.path(), &turns);
    create_tasks(&core, &["aw10-r", "aw10-a"]);
    let client = luna_rpc::memory_client(core.rpc_service());
    set_limit(&client, Some(1)).await;

    send(&client, "aw10-r", "aw10-send-r", "overload romeo").await;
    let scheduled = eventually("a scheduled retry", || {
        recovery(&core, "aw10-r").filter(|recovery| recovery.status == RecoveryStatus::Scheduled)
    })
    .await;
    send(&client, "aw10-a", "aw10-send-a", "hold alpha").await;
    eventually("alpha holds the only slot", || {
        core.sessions.turn_in_flight("aw10-a").then_some(())
    })
    .await;

    let held = eventually("the due retry waits for a slot", || {
        recovery(&core, "aw10-r").filter(|recovery| {
            recovery.status == RecoveryStatus::Scheduled
                && recovery.detail.contains("Waiting for a free turn")
        })
    })
    .await;
    assert_eq!(held.chain_id, scheduled.chain_id);
    assert_eq!(held.attempt_count, 0, "the chain was never claimed");
    let full = read_capacity(&client).await;
    assert!(
        full.waiting.iter().any(|wait| wait.task_id == "aw10-r"
            && wait.kind == HostCapacityWaitKind::AutomaticRetry
            && wait.source_id == scheduled.chain_id),
        "{full:?}"
    );
    assert_eq!(
        turns.count("romeo"),
        1,
        "the retry is not sent while the host is full"
    );

    let cancel = command(
        &client,
        "aw10-r",
        "aw10-cancel-r",
        json!({ "kind": "cancelRecovery", "chainId": scheduled.chain_id }),
    )
    .await;
    assert_eq!(cancel.status, CommandReceiptStatus::Applied, "{cancel:?}");
    let cancelled = eventually("the retry is cancelled", || {
        recovery(&core, "aw10-r").filter(|recovery| recovery.status == RecoveryStatus::Cancelled)
    })
    .await;
    assert!(
        core.sessions.host_capacity().waiting.is_empty(),
        "a cancelled retry leaves the line"
    );
    turns.release("alpha");
    eventually("alpha's turn ends", || {
        (!core.sessions.turn_in_flight("aw10-a")).then_some(())
    })
    .await;
    settle().await;
    assert_eq!(
        turns.count("romeo"),
        1,
        "a freed slot never sends a cancelled retry"
    );
    core.shutdown().await;
    drop(client);
    drop(core);

    let reopened = assemble(root.path(), &turns);
    let restored = eventually("the cancellation is restored", || {
        recovery(&reopened, "aw10-r")
            .filter(|recovery| recovery.status == RecoveryStatus::Cancelled)
    })
    .await;
    assert_eq!(restored.chain_id, scheduled.chain_id);
    settle().await;
    assert_eq!(
        turns.count("romeo"),
        1,
        "restart does not retry a cancelled chain"
    );

    evidence(
        "retry-held-for-capacity.json",
        json!({
            "scheduled": scheduled,
            "heldForCapacity": held,
            "line": full,
            "cancel": cancel,
            "cancelled": cancelled,
            "afterRestart": restored,
            "providerPrompts": *turns.prompts.lock().unwrap(),
        }),
    );
    reopened.shutdown().await;
}

/// C1, C3: a queued follow-up of an idle task waits Held and unclaimed while
/// the host is full, and starts exactly once when a slot frees.
#[tokio::test]
async fn queued_follow_up_waits_unclaimed_while_the_host_is_full() {
    let root = tempfile::tempdir().unwrap();
    let turns = Turns::default();
    let core = assemble(root.path(), &turns);
    create_tasks(&core, &["aw10-q", "aw10-a"]);
    let client = luna_rpc::memory_client(core.rpc_service());
    set_limit(&client, Some(2)).await;
    send(&client, "aw10-q", "aw10-send-q", "hold quebec").await;
    send(&client, "aw10-a", "aw10-send-a", "hold alpha").await;
    eventually("two active turns", || {
        (core.sessions.turn_in_flight("aw10-q") && core.sessions.turn_in_flight("aw10-a"))
            .then_some(())
    })
    .await;
    let queued = command(
        &client,
        "aw10-q",
        "aw10-queue-q",
        json!({
            "kind": "queueMessage",
            "messageId": "aw10-queued-row",
            "text": "queued uniform",
        }),
    )
    .await;
    assert_eq!(queued.status, CommandReceiptStatus::Applied, "{queued:?}");
    set_limit(&client, Some(1)).await;

    turns.release("quebec");
    eventually("the queued row waits for a slot", || {
        core.sessions
            .host_capacity()
            .waiting
            .iter()
            .any(|wait| {
                wait.kind == HostCapacityWaitKind::QueuedMessage
                    && wait.source_id == "aw10-queued-row"
            })
            .then_some(())
    })
    .await;
    let full = read_capacity(&client).await;
    let row = core
        .doc_host
        .queued_message("aw10-q", "aw10-queued-row")
        .unwrap()
        .expect("the waiting row stays in its queue");
    assert!(
        matches!(row.status, luna_doc::QueueMessageStatus::Held),
        "a waiting row is never claimed: {:?}",
        row.status
    );
    settle().await;
    assert_eq!(
        turns.count("uniform"),
        0,
        "a full host does not send the row"
    );

    turns.release("alpha");
    eventually("the queued row leaves its queue", || {
        core.doc_host
            .queued_message("aw10-q", "aw10-queued-row")
            .unwrap()
            .is_none()
            .then_some(())
    })
    .await;
    assert_eq!(turns.count("uniform"), 1, "the row starts exactly once");
    assert!(core.sessions.host_capacity().waiting.is_empty());

    evidence(
        "queued-row-held.json",
        json!({
            "line": full,
            "heldRow": serde_json::to_value(&row).unwrap(),
            "providerPrompts": *turns.prompts.lock().unwrap(),
        }),
    );
    turns.release("uniform");
    core.shutdown().await;
}
