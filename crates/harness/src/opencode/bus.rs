//! Bus event routing: one normalized payload at a time into the run's
//! feeds, children, permissions and questions.

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
    /// The root started an execution LUNA did not prompt (a background
    /// completion's wake): a provider-initiated turn.
    ProviderTurn,
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
    /// Native child sessions and background jobs (`subagents`).
    pub(super) children: &'a mut ChildTree,
    pub(super) turn: &'a mut TurnState,
    pub(super) pending_usage: &'a mut Option<AgentEvent>,
    pub(super) context_windows: &'a HashMap<String, u64>,
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
        turn,
        pending_usage,
        context_windows,
    } = ctx;
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    if kind.is_empty() {
        return BusOutcome::Continue;
    }
    let props = event.get("properties").unwrap_or(&Value::Null);
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
            // A child at ANY depth: its parent is the root or a session of
            // this run's tree. Each binds to the spawn chip that created it.
            let info = props.get("info").unwrap_or(&Value::Null);
            let (Some(child_id), Some(parent)) = (
                info.get("id").and_then(Value::as_str),
                info.get("parentID").and_then(Value::as_str),
            ) else {
                return BusOutcome::Continue;
            };
            let title = info
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let events = children.on_session_created(session_id, child_id, parent, title);
            forward(event_tx, events).await
        }
        OBSERVE => {
            let observed = children.observe(session_id, props, turn.active);
            if let BusOutcome::ConsumerGone = forward(event_tx, observed.events).await {
                return BusOutcome::ConsumerGone;
            }
            if observed.wake_started {
                BusOutcome::ProviderTurn
            } else {
                BusOutcome::Continue
            }
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
            if let Some(child) = children.runs.get_mut(session) {
                child
                    .feed
                    .assistant_messages
                    .entry(message.to_owned())
                    .or_insert(role == "assistant");
                let events = replay_pending(&mut child.feed, message, false, turn);
                let tagged = children.tagged(session, events);
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
                // Spawn chips register and settle from the raw v2
                // observation of the same frame (`subagents`).
                let events = part_snapshot_events(main_feed, part, true);
                mark_content(turn, &events);
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.runs.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = part_snapshot_events(&mut child.feed, part, false);
                let tagged = children.tagged(session, events);
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
            if let Some(child) = children.runs.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = part_delta_events(&mut child.feed, props, part_id, delta);
                let tagged = children.tagged(session, events);
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "permission.asked" => {
            handle_permission_asked(PermissionAsk {
                props,
                event_session,
                session_id,
                children: &children.runs,
                unbound_children: &children.unbound,
                server,
                dir,
                event_tx,
                request_input,
                auto_approve,
                permission_policy,
            })
            .await
        }
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
