//! Bus event routing: one normalized payload at a time into the run's
//! feeds, children, permissions and forms.

use super::*;

/// Provider-retry surfacing: report at 3 (a chip; the turn keeps trying),
/// abort at 8 (opencode itself would retry forever — #169's silent loop).
pub(super) const RETRY_REPORT_ATTEMPT: u64 = 3;
pub(super) const RETRY_ABORT_ATTEMPT: u64 = 8;

pub(super) enum BusOutcome {
    Continue,
    /// Our session's turn reached idle.
    TurnIdle,
    TurnInterrupted,
    ConsumerGone,
}

pub(super) type RequestInput = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

pub(super) struct BusCtx<'a> {
    pub(super) event: &'a Value,
    pub(super) session_id: &'a str,
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) request_input: &'a Arc<RequestInput>,
    pub(super) auto_approve: bool,
    pub(super) permission_policy: &'a luna_proto::PermissionPolicy,
    pub(super) main_feed: &'a mut SessionFeed,
    pub(super) children: &'a mut HashMap<String, ChildRun>,
    pub(super) pending_spawns: &'a mut VecDeque<PendingSpawn>,
    pub(super) unbound_children: &'a mut HashMap<String, String>,
    pub(super) turn: &'a mut TurnState,
    pub(super) pending_usage: &'a mut Option<AgentEvent>,
    pub(super) context_windows: &'a HashMap<String, u64>,
    /// Owned sessions, applied rules, and pending asks and forms.
    pub(super) requests: &'a mut UserRequests,
}

/// Wrap an event as subagent-attributed traffic.
pub(super) fn tag(parent: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Subagent {
        parent_tool_use_id: parent.to_owned(),
        event: Box::new(event),
    }
}

pub(super) async fn settle_children(
    children: &mut HashMap<String, ChildRun>,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    status: DoneStatus,
) {
    for child in children.values_mut() {
        if child.done {
            continue;
        }
        child.done = true;
        let _ = event_tx
            .send(Ok(tag(
                &child.parent_tool_use_id,
                AgentEvent::Done {
                    status,
                    result: None,
                    error: None,
                    session_id: None,
                },
            )))
            .await;
    }
}

/// Route one normalized bus payload (`{type, properties}`, the internal
/// shape [`normalize_v2_frame`] produces).
pub(super) async fn handle_bus_event(ctx: BusCtx<'_>) -> BusOutcome {
    let BusCtx {
        event,
        session_id,
        server,
        dir,
        event_tx,
        request_input,
        auto_approve,
        permission_policy,
        main_feed,
        children,
        pending_spawns,
        unbound_children,
        turn,
        pending_usage,
        context_windows,
        requests,
    } = ctx;
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    if kind.is_empty() {
        return BusOutcome::Continue;
    }
    let props = event.get("properties").unwrap_or(&Value::Null);
    // Any descendant session is capped at the run's permission mode as soon
    // as it exists (child binding below is separate).
    if kind == "session.created"
        && let Some(info) = props.get("info")
    {
        requests.observe_session_created(info, server, dir, event_tx);
    }
    let event_session = props
        .get("sessionID")
        .and_then(Value::as_str)
        .or_else(|| {
            props
                .get("info")
                .and_then(|i| i.get("sessionID").or_else(|| i.get("id")))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            props
                .get("part")
                .and_then(|p| p.get("sessionID"))
                .and_then(Value::as_str)
        });

    let is_ours = event_session == Some(session_id);
    if is_ours && kind == "session.interrupted" {
        return BusOutcome::TurnInterrupted;
    }
    let status = props
        .get("status")
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str);
    if is_ours && (kind == "session.idle" || (kind == "session.status" && status == Some("idle"))) {
        // OpenCode can emit both idle encodings for one completion. The first
        // may submit a queued prompt before we consume the second. Until that
        // prompt starts (or fails/gets aborted), the second is stale. It must
        // neither complete the prompt nor disarm its startup watchdog.
        return if turn.idle_ready || turn.error.is_some() {
            BusOutcome::TurnIdle
        } else {
            BusOutcome::Continue
        };
    }
    if is_ours && turn.active {
        turn.note_activity();
    }

    match kind {
        "session.status" if is_ours => {
            let status = props.get("status").unwrap_or(&Value::Null);
            match status.get("type").and_then(Value::as_str) {
                Some("busy") => turn.idle_ready = true,
                Some("retry") => {
                    turn.idle_ready = true;
                    let attempt = status.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                    let message = status
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("provider error");
                    if attempt >= RETRY_ABORT_ATTEMPT && !turn.aborted_for_retry {
                        turn.aborted_for_retry = true;
                        turn.error = Some(format!(
                            "the provider kept failing after {attempt} attempts: {message}"
                        ));
                        let msg = format!(
                            "Giving up after {attempt} provider retries: {message}. {STALL_HINT}"
                        );
                        if !send(event_tx, AgentEvent::Error { message: msg }).await {
                            return BusOutcome::ConsumerGone;
                        }
                        let _ = server.interrupt_session(session_id, dir).await;
                    } else if attempt >= RETRY_REPORT_ATTEMPT && !turn.retry_reported {
                        turn.retry_reported = true;
                        let msg = format!(
                            "The provider is failing and opencode is retrying (attempt \
                             {attempt}): {message}"
                        );
                        if !send(event_tx, AgentEvent::Error { message: msg }).await {
                            return BusOutcome::ConsumerGone;
                        }
                    }
                }
                _ => {}
            }
            BusOutcome::Continue
        }
        "session.error" | "session.warning" => {
            // Errors are session-scoped but a missing id still concerns us
            // (global provider failures).
            if event_session.is_some() && !is_ours {
                return BusOutcome::Continue;
            }
            let error = props.get("error").unwrap_or(&Value::Null);
            let name = error.get("name").and_then(Value::as_str).unwrap_or("");
            if name == "MessageAbortedError" {
                // The abort echo of an interrupt — not an error chip.
                return BusOutcome::Continue;
            }
            let message = error
                .get("data")
                .and_then(|d| d.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if name.is_empty() {
                        "opencode reported an error".to_owned()
                    } else {
                        name.to_owned()
                    }
                });
            // opencode emits the same failure twice (once bare, once wrapped
            // with an exception-name/stack prefix) — one chip per distinct
            // failure: dedupe when either first line contains the other.
            let first = |m: &str| m.lines().next().unwrap_or(m).trim().to_owned();
            let line = first(&message);
            let duplicate = turn.error.as_deref().is_some_and(|prev| {
                let prev = first(prev);
                !line.is_empty() && (prev.contains(&line) || line.contains(&prev))
            });
            if kind == "session.error" {
                turn.error = Some(message.clone());
            }
            if !duplicate && !send(event_tx, AgentEvent::Error { message }).await {
                return BusOutcome::ConsumerGone;
            }
            BusOutcome::Continue
        }
        "session.created" => {
            let info = props.get("info").unwrap_or(&Value::Null);
            // Only DIRECT children bind; a grandchild's parentID is the
            // child's session and renders inside the child's own doc.
            if info.get("parentID").and_then(Value::as_str) != Some(session_id) {
                return BusOutcome::Continue;
            }
            let Some(child_id) = info.get("id").and_then(Value::as_str) else {
                return BusOutcome::Continue;
            };
            if children.contains_key(child_id) {
                return BusOutcome::Continue;
            }
            let title = info
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !bind_child(children, pending_spawns, child_id, title) {
                unbound_children.insert(child_id.to_owned(), title.to_owned());
            }
            BusOutcome::Continue
        }
        "message.updated" => {
            let info = props.get("info").unwrap_or(&Value::Null);
            let (Some(session), Some(message), Some(role)) = (
                info.get("sessionID").and_then(Value::as_str),
                info.get("id").and_then(Value::as_str),
                info.get("role").and_then(Value::as_str),
            ) else {
                return BusOutcome::Continue;
            };
            if session == session_id {
                main_feed
                    .assistant_messages
                    .entry(message.to_owned())
                    .or_insert(role == "assistant");
                if role == "assistant" {
                    turn.provider_checkpoint_id = Some(message.to_owned());
                }
                // Token usage rides the assistant message; the last one
                // before idle wins, emitted right before Done.
                if role == "assistant"
                    && let Some(tokens) = info.get("tokens")
                {
                    if let Some(usage) = context_usage_event(info, context_windows)
                        && !send(event_tx, usage).await
                    {
                        return BusOutcome::ConsumerGone;
                    }
                    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
                    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
                    if input > 0 || output > 0 {
                        *pending_usage = Some(AgentEvent::Usage {
                            input_tokens: input,
                            output_tokens: output,
                        });
                    }
                }
                let events = replay_pending(main_feed, message, true, turn);
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.get_mut(session) {
                child
                    .feed
                    .assistant_messages
                    .entry(message.to_owned())
                    .or_insert(role == "assistant");
                // A NEW user message on a settled child is a steer resuming
                // it: un-latch so resumed traffic streams to the same chip.
                if role == "user" && child.done {
                    child.done = false;
                }
                let events = replay_pending(&mut child.feed, message, false, turn);
                let parent = child.parent_tool_use_id.clone();
                let tagged = events.into_iter().map(|ev| tag(&parent, ev)).collect();
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "message.part.updated" => {
            let part = props.get("part").unwrap_or(&Value::Null);
            let Some(session) = part.get("sessionID").and_then(Value::as_str) else {
                return BusOutcome::Continue;
            };
            if session == session_id {
                let events = part_snapshot_events(
                    main_feed,
                    part,
                    true,
                    Some((children, pending_spawns, unbound_children)),
                );
                mark_content(turn, &events);
                let mut settle: Vec<AgentEvent> = Vec::new();
                // A completed `task` part settles its child chip.
                if let Some((child_session, failed)) = task_completion(part) {
                    let by_meta = children
                        .get_mut(&child_session)
                        .map(|c| (child_session.clone(), c));
                    let target = match by_meta {
                        Some(v) => Some(v),
                        None => {
                            // No metadata binding: settle whichever child
                            // streams to this part's chip.
                            let part_id = part.get("id").and_then(Value::as_str).unwrap_or("");
                            children
                                .iter_mut()
                                .find(|(_, c)| c.parent_tool_use_id == part_id)
                                .map(|(id, c)| (id.clone(), c))
                        }
                    };
                    if let Some((_, child)) = target
                        && !child.done
                    {
                        child.done = true;
                        settle.push(tag(
                            &child.parent_tool_use_id,
                            AgentEvent::Done {
                                status: if failed {
                                    DoneStatus::Errored
                                } else {
                                    DoneStatus::Completed
                                },
                                result: None,
                                error: None,
                                session_id: None,
                            },
                        ));
                    }
                }
                let mut all = events;
                all.extend(settle);
                return forward(event_tx, all).await;
            }
            if let Some(child) = children.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = part_snapshot_events(&mut child.feed, part, false, None);
                let parent = child.parent_tool_use_id.clone();
                let tagged = events.into_iter().map(|ev| tag(&parent, ev)).collect();
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "message.part.delta" => {
            let (Some(session), Some(part_id), Some(delta)) = (
                props.get("sessionID").and_then(Value::as_str),
                props.get("partID").and_then(Value::as_str),
                props.get("delta").and_then(Value::as_str),
            ) else {
                return BusOutcome::Continue;
            };
            if props.get("field").and_then(Value::as_str) != Some("text") {
                return BusOutcome::Continue;
            }
            if session == session_id {
                let events = part_delta_events(main_feed, props, part_id, delta);
                mark_content(turn, &events);
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = part_delta_events(&mut child.feed, props, part_id, delta);
                let parent = child.parent_tool_use_id.clone();
                let tagged = events.into_iter().map(|ev| tag(&parent, ev)).collect();
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "permission.asked" => {
            // A request waiting on the user is not a stall.
            if event_session.is_some_and(|session| requests.owns(session)) {
                turn.note_activity();
            }
            handle_permission_asked(PermissionAsk {
                props,
                event_session,
                children,
                unbound_children,
                server,
                dir,
                event_tx,
                request_input,
                auto_approve,
                permission_policy,
                requests,
            })
        }
        "permission.replied" => handle_permission_replied(props, requests),
        "session.permissions" => {
            requests.observe_session_permissions(props, server, dir, event_tx);
            BusOutcome::Continue
        }
        "form.created" => {
            if event_session.is_some_and(|owner| owner == GLOBAL_FORM_OWNER || requests.owns(owner))
            {
                turn.note_activity();
            }
            handle_form_created(FormAsk {
                props,
                server,
                dir,
                event_tx,
                request_input,
                requests,
                turn_active: turn.active,
            })
        }
        "form.replied" | "form.cancelled" => handle_form_settled(props, requests),
        _ => BusOutcome::Continue,
    }
}

pub(super) async fn forward(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    events: Vec<AgentEvent>,
) -> BusOutcome {
    for ev in events {
        if event_tx.send(Ok(ev)).await.is_err() {
            return BusOutcome::ConsumerGone;
        }
    }
    BusOutcome::Continue
}

pub(super) fn mark_content(turn: &mut TurnState, events: &[AgentEvent]) {
    if turn.active
        && events.iter().any(|ev| {
            matches!(
                ev,
                AgentEvent::TextDelta { .. }
                    | AgentEvent::ReasoningDelta { .. }
                    | AgentEvent::ToolCall { .. }
            )
        })
    {
        turn.saw_content = true;
    }
}
