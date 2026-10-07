//! Bus event routing: one normalized payload at a time into the run's
//! feeds, children, permissions and forms.

use super::*;

pub(super) enum BusOutcome {
    Continue,
    /// Our session's execution succeeded (idle).
    TurnIdle,
    /// Our session's execution failed: the turn settles Errored even after
    /// content (the error chip was already emitted).
    TurnFailed,
    /// Our session's execution was interrupted (`turn.interrupt_reason`
    /// holds a non-user reason).
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
    /// Owned sessions, applied rules, and pending asks and forms.
    pub(super) requests: &'a mut UserRequests,
    /// The run's own bus channel: live shell output pages re-enter the loop
    /// through it, so their activity stays ordered before the result.
    pub(super) bus_tx: &'a mpsc::Sender<BusMsg>,
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
        requests,
        bus_tx,
    } = ctx;
    let tool_ctx = ToolSideCtx {
        server,
        dir,
        bus_tx,
    };
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
        turn.interrupt_reason = props
            .get("reason")
            .and_then(Value::as_str)
            .filter(|reason| *reason != "user")
            .map(str::to_owned);
        return BusOutcome::TurnInterrupted;
    }
    let status = props
        .get("status")
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str);
    if is_ours
        && (kind == "session.idle"
            || kind == "session.failed"
            || (kind == "session.status" && status == Some("idle")))
    {
        // A terminal from the previous busy period can trail a just-posted
        // prompt. Until that prompt's execution starts (or fails/gets
        // aborted), it is stale: it must neither complete the prompt nor
        // disarm its startup watchdog.
        if !(turn.idle_ready || turn.error.is_some()) {
            return BusOutcome::Continue;
        }
        return if kind == "session.failed" {
            turn.failed = true;
            BusOutcome::TurnFailed
        } else {
            BusOutcome::TurnIdle
        };
    }
    if is_ours && turn.active {
        turn.note_activity();
    }

    match kind {
        // v2 has no retry status (retries arrive as `session.retry`, a
        // transient notice handled by the turn loop): busy is all that
        // remains of the 1.x status vocabulary.
        "session.status" if is_ours => {
            if status == Some("busy") {
                turn.idle_ready = true;
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
                // A shell's final output drain precedes its result. Spawn
                // chips register and settle from the raw v2 observation of
                // the same frame (`subagents`).
                let mut events = prepare_tool_part(main_feed, part, &tool_ctx).await;
                events.extend(part_snapshot_events(main_feed, part, true));
                mark_content(turn, &events);
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.runs.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let mut events = prepare_tool_part(&mut child.feed, part, &tool_ctx).await;
                events.extend(part_snapshot_events(&mut child.feed, part, false));
                let tagged = children.tagged(session, events);
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        // Tool arguments being generated: a pure liveness heartbeat on our
        // own session (an empty reasoning delta, as Claude's input deltas).
        "tool.input.delta" => {
            if is_ours {
                return forward(
                    event_tx,
                    vec![AgentEvent::ReasoningDelta {
                        text: String::new(),
                    }],
                )
                .await;
            }
            BusOutcome::Continue
        }
        // Running-call progress and live shell output pages.
        "tool.progress" | "tool.shell.output" => {
            let Some(session) = event_session else {
                return BusOutcome::Continue;
            };
            let side = |feed: &mut SessionFeed| {
                if kind == "tool.progress" {
                    tool_progress_events(feed, props, &tool_ctx)
                } else {
                    shell_output_events(feed, props)
                }
            };
            if session == session_id {
                return forward(event_tx, side(main_feed)).await;
            }
            if let Some(child) = children.runs.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = side(&mut child.feed);
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
            // A request waiting on the user is not a stall.
            if event_session.is_some_and(|session| requests.owns(session)) {
                turn.note_activity();
            }
            handle_permission_asked(PermissionAsk {
                props,
                event_session,
                children: &children.runs,
                unbound_children: &children.unbound,
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
