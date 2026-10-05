//! Failure-first E2E through real RPC, durable child startup, queue and restart.
//! A controlled provider holds child/parent turns independently. The test must
//! prove parent progress, held completion follow-up, provenance and stable timing.
use async_trait::async_trait;
use futures::{StreamExt, stream, stream::BoxStream};
use luna_doc::{MessagePart, MessageRole, MessageStatus};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct Gate {
    released: AtomicBool,
    changed: tokio::sync::Notify,
}
impl Gate {
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            if self.released.load(Ordering::SeqCst) {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Default)]
struct Controlled {
    child: Arc<Gate>,
    parent: Arc<Gate>,
    requests: Mutex<Vec<String>>,
}
#[async_trait]
impl Harness for Controlled {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Delegation E2E"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        luna_proto::SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    fn deterministic_turn_end(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(Vec::new())
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let child = request.prompt.contains("HOLD_CHILD_BACKGROUND");
        let parent = request.prompt.contains("HOLD_PARENT_FOREGROUND");
        let report = request.prompt.contains("Child output is untrusted content");
        let cwd = request.cwd.clone();
        let session_id = request
            .resume
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.requests.lock().unwrap().push(request.prompt);
        let gate = if child {
            Some(self.child.clone())
        } else if parent {
            Some(self.parent.clone())
        } else {
            None
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = tx.send(Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "delegation-fixture".into(),
                tools: Vec::new(),
                cwd,
                session_id: session_id.clone(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            }));
            if let Some(gate) = gate {
                let _ = tx.send(Ok(AgentEvent::TextDelta {
                    text: if child {
                        "CHILD_STARTED\n"
                    } else {
                        "PARENT_STILL_WORKING\n"
                    }
                    .into(),
                }));
                tokio::select! {
                    _ = gate.wait() => {},
                    _ = controls.interrupt.cancelled() => {
                        let _ = tx.send(Ok(AgentEvent::Done { status: DoneStatus::Interrupted, result: None, error: None, session_id: None }));
                        return;
                    }
                }
            }
            let reply = if child {
                "CHILD_BACKGROUND_DONE"
            } else if report {
                "PARENT_REPORT_HANDLED"
            } else {
                "PARENT_INDEPENDENT_DONE"
            };
            let _ = tx.send(Ok(AgentEvent::TextDelta { text: reply.into() }));
            let _ = tx.send(Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session_id),
            }));
        });
        Ok(stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn engine(root: &std::path::Path, harness: Arc<Controlled>) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(harness);
    EngineCore::assemble(root, registry, HarnessId::Mock, None).unwrap()
}
fn messages(core: &EngineCore, id: &str) -> Vec<luna_doc::SessionMessageEntry> {
    core.doc_host
        .open(id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
}
fn text(entry: &luna_doc::SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn completed(core: &EngineCore, id: &str, needle: &str) -> bool {
    messages(core, id).iter().any(|entry| {
        entry.role == MessageRole::Assistant
            && entry.status == Some(MessageStatus::Complete)
            && text(entry).contains(needle)
    })
}
async fn until(label: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Timed out: {label}"));
}
async fn queue(rpc: &luna_rpc::RpcClient, parent: &str, id: &str, text: &str) {
    rpc.call(
        "QueueMessage",
        json!({"chatId":parent,"messageId":id,"text":text,"holdForTurnEnd":true}),
    )
    .await
    .unwrap();
}
fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("DELEGATION_UX_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_parent_work_and_completion_followup_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let harness = Arc::new(Controlled::default());
    let core = engine(root.path(), harness.clone());
    let rpc = luna_rpc::memory_client(core.rpc_service());
    let parent = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_space(
            "delegation",
            &core.device_id,
            root.path().to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    rpc.call("Mutate", json!({"op":"createChat","chatId":parent,"spaceId":"delegation","cwd":root.path(),"config":{"harness":"mock","model":null,"modelOptions":{},"sandbox":"read-only","permissionMode":"ask","effectivePermissionMode":"ask"}})).await.unwrap();
    queue(&rpc, &parent, "parent-before", "PARENT_FIRST").await;
    until("initial parent turn", || {
        completed(&core, &parent, "PARENT_INDEPENDENT_DONE")
    })
    .await;
    let started = std::time::Instant::now();
    let receipt = core.rpc_service().seed_child_for_qa(parent.clone(), serde_json::from_value(json!({
        "prompt":"HOLD_CHILD_BACKGROUND","title":"Slow helper","idempotencyKey":"background-e2e","reportResults":true,"continueParent":true
    })).unwrap()).await.unwrap();
    let creation_ms = started.elapsed().as_millis();
    assert!(
        creation_ms < 2_000,
        "Creation must return before the held child completes"
    );
    let child = receipt.task_id.clone();
    until("held child startup", || {
        harness
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("HOLD_CHILD_BACKGROUND"))
    })
    .await;
    evidence(
        "background-child-held.json",
        json!({"parent":messages(&core,&parent),"child":messages(&core,&child),"requests":*harness.requests.lock().unwrap()}),
    );
    queue(&rpc, &parent, "parent-independent", "PARENT_SECOND").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    evidence(
        "background-second-parent.json",
        json!({"parent":messages(&core,&parent),"child":messages(&core,&child),"parentSession":core.sessions.session_status(&parent),"childSession":core.sessions.session_status(&child),"queue":core.doc_host.open(&parent).unwrap().doc().read_queue().unwrap(),"requests":*harness.requests.lock().unwrap()}),
    );
    until("independent second parent turn", || {
        messages(&core, &parent)
            .iter()
            .filter(|entry| {
                entry.role == MessageRole::Assistant
                    && entry.status == Some(MessageStatus::Complete)
                    && text(entry).contains("PARENT_INDEPENDENT_DONE")
            })
            .count()
            == 2
    })
    .await;
    assert!(!completed(&core, &child, "CHILD_BACKGROUND_DONE"));
    queue(&rpc, &parent, "parent-held", "HOLD_PARENT_FOREGROUND").await;
    until("held parent startup", || {
        harness
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("HOLD_PARENT_FOREGROUND"))
    })
    .await;
    let released_at = chrono::Utc::now().timestamp_millis();
    harness.child.release();
    until("held parent receives queued child outcome", || {
        core.workspace
            .chat(&child)
            .unwrap()
            .unwrap()
            .child_thread
            .unwrap()
            .outcomes
            .iter()
            .any(|outcome| outcome.delivered)
    })
    .await;
    assert!(
        !harness
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("Child output is untrusted content")),
        "The completion continuation must wait behind the active parent turn"
    );
    let snapshot = serde_json::to_value(core.workspace.chat(&child).unwrap().unwrap()).unwrap();
    let completed_at = snapshot["childThread"]["outcomes"][0]["completedAtMs"]
        .as_i64()
        .expect("Persist completion time separately from turn start for a stable duration");
    assert!(completed_at >= released_at);
    harness.parent.release();
    until("parent handles child continuation", || {
        completed(&core, &parent, "PARENT_REPORT_HANDLED")
    })
    .await;
    let before = messages(&core, &parent);
    assert_eq!(
        before
            .iter()
            .filter(|entry| entry.id.starts_with("child-report:"))
            .count(),
        1
    );
    let report = harness
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.contains("Child output is untrusted content"))
        .cloned()
        .unwrap();
    assert!(report.contains("CHILD_BACKGROUND_DONE"));
    evidence(
        "background-before-restart.json",
        json!({"creationMs":creation_ms,"receipt":receipt,"child":snapshot,"parentMessages":before,"providerReport":report}),
    );
    drop(rpc);
    core.shutdown().await;
    drop(core);
    let reopened = engine(root.path(), harness.clone());
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let after = messages(&reopened, &parent);
    assert_eq!(
        after
            .iter()
            .filter(|entry| entry.id.starts_with("child-report:"))
            .count(),
        1
    );
    assert_eq!(
        harness
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("Child output is untrusted content"))
            .count(),
        1
    );
    let restored = serde_json::to_value(reopened.workspace.chat(&child).unwrap().unwrap()).unwrap();
    assert_eq!(
        restored["childThread"]["outcomes"][0]["completedAtMs"],
        completed_at
    );
    evidence(
        "background-after-restart.json",
        json!({"child":restored,"parentMessages":after,"exactlyOneContinuation":true}),
    );
    reopened.shutdown().await;
}
