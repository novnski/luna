//! Chronological transcript E2E through the real RPC, run loop, durable child
//! startup and child sweeper. A controlled provider holds the parent turn at
//! the exact points where the sweeper writes the "created" card and the
//! child's result, so the parent transcript must read in the order things
//! happened instead of growing one reply above every notice.
//!
//! Failure cases, written before the fix:
//! 1. A notice written mid-turn stays BELOW output the agent produced after
//!    it (the reported bug: tools and text keep updating above the cards).
//! 2. The split lands inside a growing text part, cutting a sentence across
//!    two entries.
//! 3. A tool still running at the split mints a second chip below the notice
//!    instead of resolving the original one above it.
//! 4. A split leaves an empty assistant entry, or one stuck in `streaming`.
//! 5. A part that arrives after the split lands back in the sealed entry.
//! 6. A notice landing during the final answer, then a bare provider message
//!    end, closes the last entry early: Done then has no entry for its
//!    checkpoint, or writes an empty stub.
//! 7. A text part that opens below a notice moves to a fresh entry with its
//!    generated ID (`t2`) while that entry's part count restarts, so a later
//!    paragraph mints the same ID and a streamed append (the UI's delta path)
//!    grows the wrong paragraph.
//! 8. A notice written after the run journaled output, but before that
//!    output's first coalesced flush, lands above it: the reply's entry is
//!    only created at the flush, below the notice, so the create call reads
//!    as if it happened after its own card.
//! 9. Two provider messages with only the provider's message end between
//!    them glue into one text or thought part (`Checking.The answer is 42.`)
//!    when no notice has buried the reply.
//! 10. The same messages from a native child (a tagged subagent stream) glue
//!     in the child's own transcript: its fold ignores the message ends the
//!     parent honors.
//!
//! `TIMELINE_ORDER_EVIDENCE_DIR` keeps each test's final parent transcript as
//! JSON.
use async_trait::async_trait;
use futures::{StreamExt, stream, stream::BoxStream};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, TranscriptFrame};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, ToolCall};
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
struct Timeline {
    /// Parent holds after its create-thread call until the card lands.
    created: Arc<Gate>,
    /// Parent holds mid-sentence until the child's result lands.
    reported: Arc<Gate>,
    /// Parent holds inside its final answer until a late helper reports.
    late: Arc<Gate>,
    child: Arc<Gate>,
    prompts: Mutex<Vec<String>>,
}

fn tool(id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        call: ToolCall::Mcp {
            server: "luna".into(),
            tool: name.into(),
            input: Some(json!({"title": "Timeline helper"})),
        },
    }
}

fn result(id: &str, output: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        id: id.into(),
        is_error: false,
        output: Some(output.into()),
        diff: None,
    }
}

fn text(text: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: text.into() }
}

fn message_end() -> AgentEvent {
    AgentEvent::AssistantMessageCompleted {
        assistant_message_id: uuid::Uuid::new_v4().to_string(),
    }
}

/// Three provider messages, none carrying its own separator: a thought
/// alone, a thought then text, then text alone.
fn back_to_back_messages() -> Vec<AgentEvent> {
    vec![
        AgentEvent::ReasoningDelta {
            text: "Reading the fixture.".into(),
        },
        message_end(),
        AgentEvent::ReasoningDelta {
            text: "Comparing the totals.".into(),
        },
        text("Checking."),
        message_end(),
        text("The answer is 42."),
        message_end(),
    ]
}

#[async_trait]
impl Harness for Timeline {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Timeline E2E"
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
        let child = request.prompt.contains("TIMELINE_CHILD");
        let split = request.prompt.contains("TIMELINE_SPLIT");
        let early_card = request.prompt.contains("TIMELINE_EARLY_CARD");
        let back_to_back = request.prompt.contains("TIMELINE_MESSAGES");
        let child_back_to_back = request.prompt.contains("TIMELINE_SUBAGENT_MESSAGES");
        let cwd = request.cwd.clone();
        let session_id = request
            .resume
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.prompts.lock().unwrap().push(request.prompt);
        let (created, reported, late, child_gate) = (
            self.created.clone(),
            self.reported.clone(),
            self.late.clone(),
            self.child.clone(),
        );
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let send = |event: AgentEvent| {
                let _ = tx.send(Ok(event));
            };
            let hold = |gate: Arc<Gate>| {
                let cancelled = controls.interrupt.clone();
                async move {
                    tokio::select! {
                        _ = gate.wait() => true,
                        _ = cancelled.cancelled() => false,
                    }
                }
            };
            send(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "timeline-fixture".into(),
                tools: Vec::new(),
                cwd,
                session_id: session_id.clone(),
                assistant_message_id: uuid::Uuid::new_v4().to_string(),
            });
            let completed = if child {
                send(text("CHILD_STARTED\n"));
                let released = hold(child_gate).await;
                if released {
                    send(text("CHILD_ANSWER_42"));
                }
                released
            } else if split {
                send(tool("lookup-1", "lookup"));
                send(tool("lookup-2", "lookup"));
                // The card buries the reply; the next text opens a part that
                // moves below it, then a second paragraph follows a call.
                if !hold(created).await {
                    false
                } else {
                    send(text("FIRST"));
                    send(tool("lookup-3", "lookup"));
                    send(text("OTHER"));
                    if !hold(reported).await {
                        false
                    } else {
                        send(text("-MORE"));
                        hold(late).await
                    }
                }
            } else if early_card {
                // The create call opens the reply and returns at once; the
                // card lands before the reply's first coalesced flush.
                send(tool("spawn-1", "create_thread"));
                send(result("spawn-1", "created"));
                let released = hold(created).await;
                if released {
                    send(text("PARENT_AFTER_CARD"));
                }
                released
            } else if back_to_back {
                back_to_back_messages().into_iter().for_each(send);
                true
            } else if child_back_to_back {
                // The same messages from a native child, tagged with the
                // spawn call that owns its transcript.
                send(AgentEvent::ToolCall {
                    id: "spawn-messages".into(),
                    call: ToolCall::Unknown {
                        name: "Agent".into(),
                        input: None,
                    },
                });
                let tag = |event| AgentEvent::Subagent {
                    parent_tool_use_id: "spawn-messages".into(),
                    event: Box::new(event),
                };
                back_to_back_messages()
                    .into_iter()
                    .for_each(|event| send(tag(event)));
                send(tag(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                }));
                send(result("spawn-messages", "child finished"));
                true
            } else {
                send(AgentEvent::ReasoningDelta {
                    text: "Planning one helper thread.".into(),
                });
                send(tool("spawn-1", "create_thread"));
                // The create call is still running when the card lands; the
                // provider message ends before its result arrives (Claude's
                // tool_use → result order).
                if !hold(created).await {
                    false
                } else {
                    send(message_end());
                    send(result("spawn-1", "created"));
                    send(text("PARENT_WAITING_"));
                    // The result lands while this sentence is still growing.
                    if !hold(reported).await {
                        false
                    } else {
                        send(text("CONTINUED"));
                        send(tool("wait-1", "wait_threads"));
                        send(result("wait-1", "helper finished"));
                        send(message_end());
                        send(text("PARENT_FINAL"));
                        // A late helper's card and result land mid-answer.
                        if !hold(late).await {
                            false
                        } else {
                            send(text(" DONE"));
                            send(AgentEvent::ProviderCheckpoint {
                                checkpoint_id: "timeline-turn".into(),
                            });
                            send(message_end());
                            true
                        }
                    }
                }
            };
            send(AgentEvent::Done {
                status: if completed {
                    DoneStatus::Completed
                } else {
                    DoneStatus::Interrupted
                },
                result: None,
                error: None,
                session_id: Some(session_id),
            });
        });
        Ok(stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn messages(core: &EngineCore, id: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
}

fn body(entry: &SessionMessageEntry) -> String {
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

fn tool_ids(entry: &SessionMessageEntry) -> Vec<(String, bool)> {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Tool { id, resolved, .. } => Some((id.clone(), *resolved)),
            _ => None,
        })
        .collect()
}

/// One readable line per entry, in transcript order.
fn outline(entries: &[SessionMessageEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            let kind = match entry.role {
                MessageRole::User => "user".to_string(),
                MessageRole::System => entry.id.split(':').next().unwrap_or("system").to_string(),
                MessageRole::Assistant => "assistant".to_string(),
            };
            let parts: Vec<String> = entry
                .parts
                .iter()
                .map(|part| match part {
                    MessagePart::Text { text, .. } if entry.role == MessageRole::Assistant => {
                        format!("text({text})")
                    }
                    MessagePart::Text { .. } => "text".into(),
                    MessagePart::Reasoning { .. } => "thought".into(),
                    MessagePart::Tool { id, resolved, .. } => {
                        format!("tool({id}{})", if *resolved { "" } else { ",running" })
                    }
                    _ => "other".into(),
                })
                .collect();
            format!("{kind} [{}] {:?}", parts.join(", "), entry.status)
        })
        .collect()
}

async fn until(label: &str, core: &EngineCore, parent: &str, mut ready: impl FnMut() -> bool) {
    if tokio::time::timeout(Duration::from_secs(20), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_err()
    {
        panic!(
            "Timed out: {label}\n{}",
            outline(&messages(core, parent)).join("\n")
        );
    }
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("TIMELINE_ORDER_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

/// A parent chat on the scripted provider with its first message queued.
async fn start_parent(
    text: &str,
) -> (
    tempfile::TempDir,
    Arc<Timeline>,
    EngineCore,
    luna_rpc::RpcClient,
    String,
) {
    let root = tempfile::tempdir().unwrap();
    let harness = Arc::new(Timeline::default());
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(harness.clone());
    let core = EngineCore::assemble(root.path(), registry, HarnessId::Mock, None).unwrap();
    let rpc = luna_rpc::memory_client(core.rpc_service());
    let parent = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_space(
            "timeline",
            &core.device_id,
            root.path().to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    rpc.call("Mutate", json!({"op":"createChat","chatId":parent,"spaceId":"timeline","cwd":root.path(),"config":{"harness":"mock","model":null,"modelOptions":{},"sandbox":"read-only","permissionMode":"ask","effectivePermissionMode":"ask"}})).await.unwrap();
    rpc.call(
        "QueueMessage",
        json!({"chatId":parent,"messageId":"timeline-ask","text":text,"holdForTurnEnd":true}),
    )
    .await
    .unwrap();
    (root, harness, core, rpc, parent)
}

#[tokio::test(flavor = "multi_thread")]
async fn notices_written_mid_turn_stay_in_chronological_order() {
    let (_root, harness, core, rpc, parent) =
        start_parent("Create one helper and report back").await;

    // The live reply must already be in the doc before anything lands below.
    until("parent streams its create call", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Streaming)
                && tool_ids(entry).iter().any(|(id, _)| id == "spawn-1")
        })
    })
    .await;
    let receipt = core
        .rpc_service()
        .seed_child_for_qa(
            parent.clone(),
            serde_json::from_value(json!({
                "prompt":"TIMELINE_CHILD","title":"Timeline helper","idempotencyKey":"timeline-order",
                "reportResults":true,"continueParent":false
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let child = receipt.task_id;
    let created_id = format!("child-created:{child}");
    until("created card lands mid-turn", &core, &parent, || {
        messages(&core, &parent)
            .iter()
            .any(|entry| entry.id == created_id)
    })
    .await;
    harness.created.release();
    until("reply continues below the card", &core, &parent, || {
        let entries = messages(&core, &parent);
        let card = entries.iter().position(|entry| entry.id == created_id);
        let waiting = entries
            .iter()
            .position(|entry| body(entry).contains("PARENT_WAITING_"));
        matches!((card, waiting), (Some(card), Some(waiting)) if waiting > card)
    })
    .await;
    harness.child.release();
    until("child result lands mid-turn", &core, &parent, || {
        messages(&core, &parent)
            .iter()
            .any(|entry| entry.id.starts_with("child-outcome:"))
    })
    .await;
    harness.reported.release();
    until("parent streams its final answer", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Streaming) && body(entry).contains("PARENT_FINAL")
        })
    })
    .await;
    let late = core
        .rpc_service()
        .seed_child_for_qa(
            parent.clone(),
            serde_json::from_value(json!({
                "prompt":"TIMELINE_CHILD late","title":"Late helper","idempotencyKey":"timeline-late",
                "reportResults":true,"continueParent":false
            }))
            .unwrap(),
        )
        .await
        .unwrap()
        .task_id;
    until(
        "late helper reports during the answer",
        &core,
        &parent,
        || {
            messages(&core, &parent)
                .iter()
                .any(|entry| entry.id.starts_with(&format!("child-outcome:{late}:")))
        },
    )
    .await;
    harness.late.release();
    until("parent turn completes", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Complete)
                && body(entry).contains("PARENT_FINAL DONE")
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let entries = messages(&core, &parent);
    let lines = outline(&entries);
    evidence(
        "timeline-order",
        json!({ "outline": lines, "entries": entries }),
    );
    let shape: Vec<&str> = lines
        .iter()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        shape,
        [
            "user",
            "assistant",
            "child-created",
            "assistant",
            "child-outcome",
            "assistant",
            "child-created",
            "child-outcome"
        ],
        "Case 1: notices must sit between the work before and after them\n{}",
        lines.join("\n")
    );
    let replies: Vec<&SessionMessageEntry> = entries
        .iter()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .collect();
    assert_eq!(
        body(replies[1]),
        "PARENT_WAITING_CONTINUED",
        "Case 2: a sentence growing across the notice stays whole\n{}",
        lines.join("\n")
    );
    assert_eq!(
        tool_ids(replies[0]),
        [("spawn-1".to_string(), true)],
        "Case 3: the create call resolves in place above the card\n{}",
        lines.join("\n")
    );
    let chips: Vec<String> = replies
        .iter()
        .flat_map(|entry| tool_ids(entry))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        chips,
        ["spawn-1", "wait-1"],
        "Case 3: no tool chip is duplicated across the split\n{}",
        lines.join("\n")
    );
    for reply in &replies {
        assert!(
            !reply.parts.is_empty() && reply.status == Some(MessageStatus::Complete),
            "Case 4: every split segment is non-empty and settled\n{}",
            lines.join("\n")
        );
    }
    assert_eq!(
        tool_ids(replies[2]),
        [("wait-1".to_string(), true)],
        "Case 5: work after the result lands below it\n{}",
        lines.join("\n")
    );
    assert_eq!(
        body(replies[2]),
        "PARENT_FINAL DONE",
        "Cases 2 and 5: the final answer reads after the result and stays whole\n{}",
        lines.join("\n")
    );
    assert_eq!(
        core.doc_host
            .open(&parent)
            .unwrap()
            .doc()
            .provider_checkpoint(&replies[2].id)
            .as_deref(),
        Some("timeline-turn"),
        "Case 6: the turn's checkpoint lands on its last written entry\n{}",
        lines.join("\n")
    );
    drop(rpc);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn split_reply_keeps_part_ids_unique_so_deltas_grow_the_right_paragraph() {
    let (_root, harness, core, rpc, parent) = start_parent("TIMELINE_SPLIT look things up").await;
    until("parent streams its two calls", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Streaming)
                && tool_ids(entry).iter().any(|(id, _)| id == "lookup-2")
        })
    })
    .await;
    let child = core
        .rpc_service()
        .seed_child_for_qa(
            parent.clone(),
            serde_json::from_value(json!({
                "prompt":"TIMELINE_CHILD","title":"Timeline helper","idempotencyKey":"timeline-split",
                "reportResults":true,"continueParent":false
            }))
            .unwrap(),
        )
        .await
        .unwrap()
        .task_id;
    let created_id = format!("child-created:{child}");
    until("created card buries the reply", &core, &parent, || {
        messages(&core, &parent)
            .iter()
            .any(|entry| entry.id == created_id)
    })
    .await;
    harness.created.release();
    until(
        "second paragraph streams below the card",
        &core,
        &parent,
        || {
            messages(&core, &parent).iter().any(|entry| {
                entry.status == Some(MessageStatus::Streaming) && body(entry).ends_with("OTHER")
            })
        },
    )
    .await;
    let before = messages(&core, &parent);
    harness.reported.release();
    until("second paragraph grows", &core, &parent, || {
        messages(&core, &parent)
            .iter()
            .any(|entry| body(entry).contains("OTHER-MORE"))
    })
    .await;
    let after = messages(&core, &parent);

    // The UI's live path: the watch diffs consecutive snapshots into a frame
    // and the transcript applies it to its copy of the earlier one.
    let frame = luna_doc::diff_transcript(&before, &after);
    let mut projected = before.clone();
    let applied = luna_doc::apply_transcript_frame(&mut projected, frame.clone());
    let lines = outline(&after);
    evidence(
        "split-part-ids",
        json!({ "outline": lines, "before": before, "after": after, "frame": frame }),
    );
    assert!(
        matches!(&frame, TranscriptFrame::Delta { append, .. } if append.len() == 1),
        "Case 7 precondition: the growth streams as one text append\n{}",
        lines.join("\n")
    );
    assert!(
        applied.is_ok() && projected == after,
        "Case 7: the append grows the paragraph that streamed it ({applied:?})\n{}",
        lines.join("\n")
    );
    for entry in &after {
        let mut ids = std::collections::HashSet::new();
        assert!(
            entry.parts.iter().all(|part| ids.insert(part.id())),
            "Case 7: part IDs stay unique within entry {}\n{}",
            entry.id,
            lines.join("\n")
        );
    }
    drop(rpc);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn notice_landing_before_the_first_flush_stays_below_journaled_output() {
    let (_root, harness, core, rpc, parent) = start_parent("TIMELINE_EARLY_CARD").await;

    // The create call and its result are journaled (subscribers have seen
    // them) well inside the reply's first STREAM_COMMIT_MS window.
    let (replay, mut live) = core.sessions.subscribe(&parent, 0).unwrap();
    let create_returned =
        |event: &AgentEvent| matches!(event, AgentEvent::ToolResult { id, .. } if id == "spawn-1");
    if !replay
        .iter()
        .any(|journaled| create_returned(&journaled.event))
    {
        tokio::time::timeout(Duration::from_secs(20), async {
            while !create_returned(&live.recv().await.unwrap().event) {}
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Timed out: create call journaled\n{}",
                outline(&messages(&core, &parent)).join("\n")
            )
        });
    }
    // The created card, written the way the child sweeper appends it: a
    // finished System entry pushed below whatever the doc holds now. The
    // sweeper itself runs on a 2 s tick and cannot be aimed inside this
    // window on demand.
    core.doc_host
        .open(&parent)
        .unwrap()
        .doc()
        .push_message(&SessionMessageEntry {
            id: "child-created:early-helper".into(),
            role: MessageRole::System,
            parts: vec![MessagePart::Text {
                id: "text".into(),
                text: "Created child Early helper. Fresh context.".into(),
            }],
            created_at: 0,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    harness.created.release();
    until("parent turn completes", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Complete)
                && body(entry).contains("PARENT_AFTER_CARD")
        })
    })
    .await;

    let entries = messages(&core, &parent);
    let lines = outline(&entries);
    evidence(
        "notice-before-first-flush",
        json!({ "outline": lines, "entries": entries }),
    );
    let shape: Vec<&str> = lines
        .iter()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        shape,
        ["user", "assistant", "child-created", "assistant"],
        "Case 8: output journaled before the card stays above it\n{}",
        lines.join("\n")
    );
    let replies: Vec<&SessionMessageEntry> = entries
        .iter()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .collect();
    assert_eq!(
        tool_ids(replies[0]),
        [("spawn-1".to_string(), true)],
        "Case 8: the create call sits above its card, resolved in place\n{}",
        lines.join("\n")
    );
    assert_eq!(
        body(replies[1]),
        "PARENT_AFTER_CARD",
        "Case 8: output after the card lands below it\n{}",
        lines.join("\n")
    );
    drop(rpc);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn back_to_back_provider_messages_stay_separate_parts() {
    let (_root, _harness, core, rpc, parent) = start_parent("TIMELINE_MESSAGES").await;
    until("reply settles", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Complete)
                && body(entry).contains("The answer is 42.")
        })
    })
    .await;

    let entries = messages(&core, &parent);
    let lines = outline(&entries);
    evidence(
        "message-boundary",
        json!({ "outline": lines, "entries": entries }),
    );
    let replies: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("assistant"))
        .collect();
    assert_eq!(
        replies,
        ["assistant [thought, thought, text(Checking.), text(The answer is 42.)] Some(Complete)"],
        "Case 9: each provider message opens its own thought or text part\n{}",
        lines.join("\n")
    );
    drop(rpc);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn back_to_back_child_messages_stay_separate_parts() {
    let (_root, _harness, core, rpc, parent) = start_parent("TIMELINE_SUBAGENT_MESSAGES").await;
    // The parent's Done follows the child's, so its settled reply means the
    // child transcript is final.
    until("parent turn completes", &core, &parent, || {
        messages(&core, &parent).iter().any(|entry| {
            entry.status == Some(MessageStatus::Complete)
                && tool_ids(entry) == [("spawn-messages".to_string(), true)]
        })
    })
    .await;

    let child = format!("{parent}--sub--spawn-messages");
    let entries = messages(&core, &child);
    let lines = outline(&entries);
    evidence(
        "child-message-boundary",
        json!({ "outline": lines, "entries": entries }),
    );
    let replies: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("assistant"))
        .collect();
    assert_eq!(
        replies,
        ["assistant [thought, thought, text(Checking.), text(The answer is 42.)] Some(Complete)"],
        "Case 10: each of a native child's provider messages opens its own part\n{}",
        lines.join("\n")
    );
    drop(rpc);
    core.shutdown().await;
}
