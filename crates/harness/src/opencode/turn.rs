//! The run loop: session setup, the first prompt, steering, stall and
//! interrupt handling, and turn settlement.

use super::*;

/// Default bound on prompt-send → first session-scoped bus event.
pub(super) const DEFAULT_STALL_BOUND: Duration = Duration::from_secs(60);
pub(super) const STALL_ENV: &str = "LUNA_OPENCODE_STALL_MS";

/// What a wedged/silent run usually means for opencode.
pub(super) const STALL_HINT: &str = "The model provider is likely unreachable or rejecting requests. \
     Check the model/provider setup (`opencode auth list`, opencode.json) or the opencode \
     log (~/.local/share/opencode/log).";

/// After the last background job of a parked run settles, how long the run
/// waits for the root wake execution its completion starts.
pub(super) const WAKE_GRACE: Duration = Duration::from_secs(10);

pub(super) fn stall_bound() -> Option<Duration> {
    match std::env::var(STALL_ENV) {
        Ok(v) => match v.parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(Duration::from_millis(ms)),
            Err(_) => Some(DEFAULT_STALL_BOUND),
        },
        Err(_) => Some(DEFAULT_STALL_BOUND),
    }
}

pub(super) struct Session {
    pub(super) server: Server,
    pub(super) event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) controls: RunControls,
    pub(super) request: RunRequest,
    pub(super) interrupt_grace: Duration,
    pub(super) kill_grace: Duration,
    pub(super) known_commands: Option<Vec<SlashCommand>>,
    /// Per-child Stop registry (`stop_native_child`).
    pub(super) live_runs: LiveRuns,
}

pub(super) fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Rotate the assistant message id; returns (previous, next).
pub(super) fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

pub(super) async fn send(
    tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    ev: AgentEvent,
) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

pub(super) struct TurnState {
    /// A prompt is in flight (busy expected/observed; idle settles it).
    pub(super) active: bool,
    /// An idle belongs to this prompt only after its busy/retry transition,
    /// or after we explicitly abort it. A trailing idle from the previous
    /// turn must not settle a just-submitted boundary steer.
    pub(super) idle_ready: bool,
    /// Bus events about our session seen since the prompt was posted.
    pub(super) saw_activity: bool,
    /// Renderable content (text/reasoning/tool) seen this turn.
    pub(super) saw_content: bool,
    /// Terminal error to fold into Done.
    pub(super) error: Option<String>,
    /// Provider-retry chip already surfaced this turn.
    pub(super) retry_reported: bool,
    /// This turn was aborted because the provider retry loop hit the cap.
    pub(super) aborted_for_retry: bool,
    /// Last provider assistant message in this root turn. OpenCode's native
    /// fork endpoint uses message ids as history checkpoints.
    pub(super) provider_checkpoint_id: Option<String>,
    /// Deadline for the first session-scoped event after the prompt.
    pub(super) stall_deadline: Option<tokio::time::Instant>,
}

impl TurnState {
    pub(super) fn begin(stall: Option<Duration>) -> Self {
        Self {
            active: true,
            idle_ready: false,
            saw_activity: false,
            saw_content: false,
            error: None,
            retry_reported: false,
            aborted_for_retry: false,
            provider_checkpoint_id: None,
            stall_deadline: stall.map(|d| tokio::time::Instant::now() + d),
        }
    }

    pub(super) fn note_activity(&mut self) {
        self.saw_activity = true;
        self.stall_deadline = None;
    }
}

pub(super) async fn run_session(session: Session) {
    let Session {
        mut server,
        event_tx,
        controls,
        request,
        interrupt_grace,
        kill_grace,
        known_commands,
        live_runs,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        ..
    } = controls;
    let request_input = Arc::new(request_input);
    let permission_policy = request.permission_policy.clone().unwrap_or_default();
    let directory = (!request.cwd.is_empty()).then(|| request.cwd.clone());
    let dir = directory.as_deref();
    let permission_rules = opencode_permission_rules(
        request
            .permission_policy
            .as_ref()
            .map(|policy| policy.effective_mode)
            .unwrap_or(luna_proto::PermissionMode::Ask),
    );

    // ---- session create/resume -------------------------------------------
    let setup = async {
        let session_id = match &request.resume {
            Some(resume) => {
                // Sessions are durable server-side: resume = reuse the id.
                // A failed lookup (including a transient server failure) is
                // not authorization to discard this conversation's history.
                let info = server.session_info(resume, dir).await?;
                info.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(resume)
                    .to_owned()
            }
            None => create_session(&server, dir, &permission_rules).await?,
        };
        if request.resume.is_some() {
            // `session.update` (204): the resumed session takes this run's
            // LUNA permission mode as its v2 ruleset.
            server
                .patch_json(
                    &format!("/api/session/{session_id}"),
                    dir,
                    &json!({ "permissions": permission_rules }),
                )
                .await?;
        }

        // Provider catalog: resolves the model's advertised reasoning
        // variants so the requested effort only rides models that have it.
        let providers = server.provider_catalog(dir).await.unwrap_or_default();

        // The requested model (+ variant) is set on the session once;
        // prompts carry only text and files.
        if let Some((provider, model_id)) = request
            .model
            .as_deref()
            .and_then(|m| m.split_once('/'))
            .map(|(p, m)| (p.to_owned(), m.to_owned()))
        {
            let variant = pick_variant(&providers, &provider, &model_id, request.reasoning);
            server
                .set_model(&session_id, &provider, &model_id, variant.as_deref(), dir)
                .await?;
        }
        Ok::<(String, ProviderCatalog), HarnessError>((session_id, providers))
    };
    let (session_id, providers) = tokio::select! {
        res = setup => match res {
            Ok(v) => v,
            Err(e) => {
                let _ = send(&event_tx, AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(e.to_string()),
                    session_id: None,
                }).await;
                server.shutdown(kill_grace).await;
                return;
            }
        },
        _ = interrupt.cancelled() => {
            let _ = send(&event_tx, AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }).await;
            server.shutdown(kill_grace).await;
            return;
        }
    };

    let context_windows: HashMap<String, u64> = providers
        .all
        .iter()
        .flatten()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .flat_map(|models| models.iter())
                .filter_map(|(id, model)| {
                    Some((
                        format!("{}/{}", provider.id.as_deref()?, id),
                        model.limit.context.filter(|n| *n > 0)?,
                    ))
                })
        })
        .collect();
    drop(providers);

    let mut assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Opencode,
            model: request.model.clone().unwrap_or_default(),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        server.shutdown(kill_grace).await;
        return;
    }
    // Per-child Stop reaches this loop through the run's registration under
    // its root session (released when the run ends).
    let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<ChildStopRequest>();
    let _live = live_runs.register(&session_id, stop_tx);

    // Advertise slash commands (composer popup); a warm cache skips the call.
    let commands = match known_commands {
        Some(commands) => commands,
        None => server
            .commands_wire(dir)
            .await
            .map(|v| commands_from_wire(&v))
            .unwrap_or_default(),
    };
    if !commands.is_empty()
        && !send(
            &event_tx,
            AgentEvent::AvailableCommands {
                commands: commands.clone(),
            },
        )
        .await
    {
        server.shutdown(kill_grace).await;
        return;
    }

    // ---- SSE bus ----------------------------------------------------------
    let (bus_tx, mut bus_rx) = mpsc::channel::<BusMsg>(256);
    // Slash-command failures re-enter the loop through the same channel.
    let command_tx = bus_tx.clone();
    let bus_handle = tokio::spawn(bus_task(server.base.clone(), server.auth.clone(), bus_tx));

    // ---- first prompt -----------------------------------------------------
    // The bus has no replay: wait for the subscription to be LIVE before
    // prompting, or a fast-failing turn's whole lifecycle can slip into the
    // gap (observed live: busy → error → idle inside ~200ms). Bounded — the
    // stall watchdog still guards a bus that never comes up.
    let connect_wait = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match bus_rx.recv().await {
                Some(BusMsg::Connected) | None => return,
                // Nothing else can arrive before Connected; drop defensively.
                Some(_) => {}
            }
        }
    })
    .await;
    if connect_wait.is_err() {
        tracing::debug!(
            target: "luna_harness::opencode",
            "event bus not connected within 15s; prompting anyway"
        );
    }
    let stall = stall_bound();
    if let Err(e) = post_prompt(
        &server,
        &session_id,
        dir,
        &commands,
        &request.prompt,
        TurnSpec {
            attachments: &request.attachments,
        },
        &command_tx,
    )
    .await
    {
        let _ = send(
            &event_tx,
            AgentEvent::Error {
                message: e.to_string(),
            },
        )
        .await;
        let _ = send(
            &event_tx,
            AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some(e.to_string()),
                session_id: Some(session_id.clone()),
            },
        )
        .await;
        bus_handle.abort();
        server.shutdown(kill_grace).await;
        return;
    }
    let mut turn = TurnState::begin(stall);

    // ---- main loop --------------------------------------------------------
    let mut main_feed = SessionFeed::default();
    // Native child sessions (any depth) and background jobs.
    let mut children = ChildTree::default();
    // The root turn settled while background work runs: the run stays alive,
    // like a persistent adapter, for that work's traffic and wake turns.
    let mut parked = false;
    // After a root Stop: a bounded wait for the children's own terminals.
    let mut drain_deadline: Option<tokio::time::Instant> = None;
    // A parked run's background work ended: the root wake its completion
    // starts should follow within this deadline, else the run ends.
    let mut wake_deadline: Option<tokio::time::Instant> = None;
    let mut queued_steers: VecDeque<String> = VecDeque::new();
    let mut steering_open = true;
    let mut interrupt_requested = false;
    let mut pending_usage: Option<AgentEvent> = None;
    let mut done_sent = false;

    // Post-abort grace: the abort endpoint promised an idle; if it never
    // arrives the run hard-stops. Unlike the stall bound this is NOT
    // disarmed by activity — only idle ends an abort.
    let mut abort_deadline: Option<tokio::time::Instant> = None;

    // Idle settlement, shared by the idle bus events and the post-reconnect
    // status re-sync (a macro so `break`/`continue` act on the caller's
    // loop): emit held usage, then Interrupted / next queued steer /
    // AssistantMessageCompleted + Done.
    macro_rules! settle_idle {
        ($label:lifetime) => {{
            if !turn.active {
                continue $label;
            }
            turn.active = false;
            if let Some(usage) = pending_usage.take()
                && !send(&event_tx, usage).await
            {
                break $label;
            }
            if interrupt_requested {
                // Children settle on their own terminals (a stopped child's
                // execution.interrupted), never locally: wait a bounded grace.
                // One that never reports stays open (the engine shows it
                // unobserved).
                if children.has_open() && drain_deadline.is_none() {
                    abort_deadline = None;
                    drain_deadline = Some(tokio::time::Instant::now() + interrupt_grace);
                    continue $label;
                }
                let _ = send(&event_tx, AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(session_id.clone()),
                }).await;
                done_sent = true;
                break $label;
            }
            if let Some(steer) = queued_steers.pop_front() {
                let (prev, next) = rotate(&mut assistant_message_id);
                if !send(&event_tx, AgentEvent::Steered {
                    user_message_id: None,
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                }).await {
                    break $label;
                }
                match post_prompt(
                    &server,
                    &session_id,
                    dir,
                    &commands,
                    &steer,
                    TurnSpec {
                        attachments: &[],
                    },
                    &command_tx,
                )
                .await
                {
                    Ok(()) => {
                        turn = TurnState::begin(stall);
                        continue $label;
                    }
                    Err(e) => {
                        let _ = send(&event_tx, AgentEvent::Error {
                            message: e.to_string(),
                        }).await;
                        // Fall through to Done below.
                    }
                }
            }
            let (prev, _next) = rotate(&mut assistant_message_id);
            if !send(&event_tx, AgentEvent::AssistantMessageCompleted {
                assistant_message_id: prev,
            }).await {
                break $label;
            }
            let errored = turn.aborted_for_retry
                || (turn.error.is_some() && !turn.saw_content);
            if let Some(checkpoint_id) = turn.provider_checkpoint_id.clone()
                && !send(&event_tx, AgentEvent::ProviderCheckpoint { checkpoint_id }).await
            {
                break $label;
            }
            let _ = send(&event_tx, AgentEvent::Done {
                status: if errored {
                    DoneStatus::Errored
                } else {
                    DoneStatus::Completed
                },
                result: None,
                error: if errored { turn.error.clone() } else { None },
                session_id: Some(session_id.clone()),
            }).await;
            done_sent = true;
            // Background subagents or shells outlive the root turn: stay for
            // their traffic and the wake turn their completion starts.
            if children.has_background_work() {
                parked = true;
                continue $label;
            }
            break $label;
        }};
    }

    'main: loop {
        // The stall watchdog only arms while a turn awaits its first sign of
        // life; a running tool's silence never trips it (events already
        // proved the turn alive and disarmed it). Computed by value each
        // iteration so the future doesn't hold a borrow of `turn`.
        let deadline = drain_deadline
            .or(abort_deadline)
            .or(wake_deadline.filter(|_| parked && !turn.active))
            .or_else(|| {
                (turn.active && !turn.saw_activity)
                    .then_some(turn.stall_deadline)
                    .flatten()
            });
        let stall_sleep = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;

            _ = interrupt.cancelled(), if !interrupt_requested => {
                interrupt_requested = true;
                // Background children are independent of the root: stop each
                // through the same per-child interrupt (foreground children
                // stop natively with the root).
                stop_background_children(&children, &server, dir);
                if turn.active {
                    turn.idle_ready = true;
                    let abort = tokio::time::timeout(
                        Duration::from_secs(5),
                        server.interrupt_session(&session_id, dir),
                    )
                    .await;
                    if !matches!(abort, Ok(Ok(true))) {
                        // `interrupted: false` means nothing is running (no
                        // terminal frame will follow); a failing endpoint
                        // means the server itself is wedged. Either way,
                        // settle now.
                        settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                        let _ = send(&event_tx, AgentEvent::Done {
                            status: DoneStatus::Interrupted,
                            result: None,
                            error: None,
                            session_id: Some(session_id.clone()),
                        }).await;
                        done_sent = true;
                        break 'main;
                    }
                    // The interrupt ends the execution with
                    // `session.execution.interrupted` — the settle path below
                    // finishes up. The grace guards a server that never
                    // delivers it.
                    abort_deadline = Some(tokio::time::Instant::now() + interrupt_grace);
                } else if children.has_open() {
                    // Parked: wait for the stopped children's own terminals.
                    parked = false;
                    wake_deadline = None;
                    drain_deadline = Some(tokio::time::Instant::now() + interrupt_grace);
                } else {
                    let _ = send(&event_tx, AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: Some(session_id.clone()),
                    }).await;
                    done_sent = true;
                    break 'main;
                }
            }

            steer = steering.recv(), if steering_open => {
                match steer {
                    Some(steer) => {
                        if steer.delivery == crate::SteerDelivery::RequireActiveTurn {
                            if let Some(tx) = steer.acceptance {
                                let _ = tx.send(crate::SteerAcceptance::Rejected(
                                    "OpenCode supports follow-up turns, not native in-turn steering"
                                        .into(),
                                ));
                            }
                            continue 'main;
                        }
                        if turn.active {
                            queued_steers.push_back(steer.prompt);
                        } else {
                            // Between turns (shouldn't happen — the engine
                            // steers live runs — but deliver, don't drop).
                            let (prev, next) = rotate(&mut assistant_message_id);
                            let _ = send(&event_tx, AgentEvent::Steered {
                                user_message_id: None,
                                assistant_message_id: Some(prev),
                                next_assistant_message_id: Some(next),
                            }).await;
                            if post_prompt(
                                &server,
                                &session_id,
                                dir,
                                &commands,
                                &steer.prompt,
                                TurnSpec {
                                    attachments: &[],
                                },
                                &command_tx,
                            )
                            .await
                            .is_ok()
                            {
                                turn = TurnState::begin(stall);
                                parked = false;
                                wake_deadline = None;
                            }
                        }
                    }
                    None => {
                        steering_open = false;
                        // The engine retires an idle runtime by closing its
                        // mailbox: a parked run ends instead of lingering.
                        if parked && !turn.active {
                            break 'main;
                        }
                    }
                }
            }

            Some(request) = stop_rx.recv() => {
                answer_stop(request, &children, &server, dir);
            }

            _ = stall_sleep => {
                if drain_deadline.is_some() {
                    // Children that never reported their terminal stay open.
                    let _ = send(&event_tx, AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: Some(session_id.clone()),
                    }).await;
                    done_sent = true;
                    break 'main;
                }
                if wake_deadline.is_some() && parked && !turn.active {
                    wake_deadline = None;
                    if children.has_background_work() {
                        // Another background job is still running.
                        continue 'main;
                    }
                    // No wake followed: nothing is left to observe.
                    break 'main;
                }
                if abort_deadline.is_some() {
                    // Abort acknowledged nothing within the grace: hard stop.
                    settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                    let _ = send(&event_tx, AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        result: None,
                        error: None,
                        session_id: Some(session_id.clone()),
                    }).await;
                    done_sent = true;
                    break 'main;
                }
                let message = format!(
                    "opencode made no progress for {}s after the prompt. {STALL_HINT}",
                    stall.unwrap_or(DEFAULT_STALL_BOUND).as_secs()
                );
                let _ = send(&event_tx, AgentEvent::Error { message: message.clone() }).await;
                let _ = server.interrupt_session(&session_id, dir).await;
                settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                let _ = send(&event_tx, AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(message),
                    session_id: Some(session_id.clone()),
                }).await;
                done_sent = true;
                break 'main;
            }

            msg = bus_rx.recv() => {
                let Some(msg) = msg else { break 'main };
                match msg {
                    BusMsg::Connected => {
                        // A RECONNECT mid-turn may have swallowed our idle
                        // (no replay): re-sync from the server's own status
                        // surface — not running means idle. Can't tell:
                        // leave the turn running; the next disconnect or
                        // event decides.
                        if turn.active
                            && !server
                                .session_running(&session_id, dir)
                                .await
                                .unwrap_or(true)
                        {
                            settle_idle!('main);
                        }
                    }
                    BusMsg::Disconnected => {
                        if parked && !turn.active {
                            // A post-turn server death: every turn already
                            // settled; open children stay unobserved.
                            break 'main;
                        }
                        let crashed = match server.child.as_mut() {
                            Some(child) => child.try_wait().ok().flatten(),
                            None => None,
                        };
                        let message = crate::crash_message(
                            "opencode serve",
                            crashed,
                            &server.stderr_tail,
                        );
                        if turn.active {
                            let _ = send(&event_tx, AgentEvent::Error { message: message.clone() }).await;
                        }
                        settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                        let _ = send(&event_tx, AgentEvent::Done {
                            status: if interrupt_requested {
                                DoneStatus::Interrupted
                            } else {
                                DoneStatus::Errored
                            },
                            result: None,
                            error: (!interrupt_requested).then_some(message),
                            session_id: Some(session_id.clone()),
                        }).await;
                        done_sent = true;
                        break 'main;
                    }
                    BusMsg::CommandFailed(message) => {
                        // A command that never started its execution would
                        // otherwise stall until the watchdog; one that did
                        // start settles from its own terminal frame.
                        if !send(&event_tx, AgentEvent::Error { message: message.clone() }).await {
                            break 'main;
                        }
                        if turn.active && !turn.saw_activity {
                            turn.error = Some(message);
                            settle_idle!('main);
                        }
                    }
                    BusMsg::Event(event) => {
                        let outcome = handle_bus_event(BusCtx {
                            event: &event,
                            session_id: &session_id,
                            server: &server,
                            dir,
                            event_tx: &event_tx,
                            request_input: &request_input,
                            auto_approve: request.auto_approve,
                            permission_policy: &permission_policy,
                            main_feed: &mut main_feed,
                            children: &mut children,
                            turn: &mut turn,
                            pending_usage: &mut pending_usage,
                            context_windows: &context_windows,
                        }).await;
                        match outcome {
                            BusOutcome::Continue => {}
                            BusOutcome::ConsumerGone => break 'main,
                            BusOutcome::TurnIdle => settle_idle!('main),
                            BusOutcome::TurnInterrupted => {
                                interrupt_requested = true;
                                settle_idle!('main);
                            }
                            BusOutcome::ProviderTurn if !interrupt_requested => {
                                // A background completion's wake: a provider
                                // turn, ending with its own Done.
                                parked = false;
                                wake_deadline = None;
                                let turn_id =
                                    format!("opencode-turn-{}", uuid::Uuid::new_v4().simple());
                                if !send(&event_tx, AgentEvent::ProviderTurnStarted {
                                    thread_id: session_id.clone(),
                                    turn_id,
                                }).await {
                                    break 'main;
                                }
                                turn = TurnState::begin(None);
                                turn.idle_ready = true;
                                turn.note_activity();
                            }
                            BusOutcome::ProviderTurn => {}
                        }
                        if drain_deadline.is_some() && !children.has_open() {
                            let _ = send(&event_tx, AgentEvent::Done {
                                status: DoneStatus::Interrupted,
                                result: None,
                                error: None,
                                session_id: Some(session_id.clone()),
                            }).await;
                            done_sent = true;
                            break 'main;
                        }
                        if parked && !turn.active {
                            // A completion reached the idle root, or the last
                            // background job ended: its wake should follow.
                            if children.take_wake_expected()
                                || (wake_deadline.is_none() && !children.has_background_work())
                            {
                                wake_deadline = Some(tokio::time::Instant::now() + WAKE_GRACE);
                            }
                        } else {
                            children.take_wake_expected();
                        }
                    }
                }
            }
        }
    }

    if !done_sent {
        // Consumer went away (stream dropped): nothing to report to.
        tracing::debug!(target: "luna_harness::opencode", "run loop ended without settling");
    }
    bus_handle.abort();
    server.shutdown(kill_grace).await;
}
