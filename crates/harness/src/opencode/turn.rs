//! The run loop: session setup, the first prompt, turn correlation, native
//! steering, stall and interrupt handling, reconnect resync and settlement.
//!
//! Turn correlation (OC-07) follows the official ACP: every prompt carries
//! a caller id (`msg_…`, the inbox id), the turn starts on
//! `session.inbox.delivered{inboxID}` and ends on the next execution
//! terminal of the session. A terminal that arrives before the turn's own
//! delivery (or with a native steer still undelivered) waits briefly for the
//! next busy period, then settles from readback. A run whose server never
//! emits inbox events falls back to execution-based correlation.
//!
//! Steering is native and step-bounded (PD-2): an explicit steer is a
//! prompt with `delivery: "steer"`, accepted on HTTP 200 and recorded (a
//! `Steered` boundary with the exact LUNA message id) on its own delivery.
//! Stop interrupts without `resume` and withdraws undelivered steers.

use super::*;

/// Default bound on prompt-send → first session-scoped bus event.
pub(super) const DEFAULT_STALL_BOUND: Duration = Duration::from_secs(60);
pub(super) const STALL_ENV: &str = "LUNA_OPENCODE_STALL_MS";
/// Default bound on silence AFTER a turn showed activity, while nothing
/// waits on the user and no tool runs (a provider stream that hung).
pub(super) const DEFAULT_ACTIVITY_STALL_BOUND: Duration = Duration::from_secs(600);
pub(super) const ACTIVITY_STALL_ENV: &str = "LUNA_OPENCODE_ACTIVITY_STALL_MS";
/// A terminal that arrived before the turn's own delivery (or with a steer
/// undelivered) waits this long for the next busy period.
pub(super) const UNDELIVERED_GRACE: Duration = Duration::from_secs(5);
/// An admitted prompt with no enqueue, busy or delivery evidence after this
/// long is checked by readback (an already-delivered id is reconciled
/// silently by OpenCode).
pub(super) const ADMISSION_GRACE: Duration = Duration::from_secs(3);
/// Message ids of the root session remembered for readback windows.
const MAX_KNOWN_MESSAGES: usize = 512;
/// Request ids remembered for reconnect dedupe.
const MAX_SEEN_REQUESTS: usize = 1024;

/// What a wedged/silent run usually means for opencode.
pub(super) const STALL_HINT: &str = "The model provider is likely unreachable or rejecting requests. \
     Check the model/provider setup (`opencode auth list`, opencode.json) or the opencode \
     log (~/.local/share/opencode/log).";

pub(super) fn stall_bound() -> Option<Duration> {
    bound_from_env(STALL_ENV, DEFAULT_STALL_BOUND)
}

pub(super) fn activity_stall_bound() -> Option<Duration> {
    bound_from_env(ACTIVITY_STALL_ENV, DEFAULT_ACTIVITY_STALL_BOUND)
}

fn bound_from_env(key: &str, default: Duration) -> Option<Duration> {
    match std::env::var(key) {
        Ok(v) => match v.parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(Duration::from_millis(ms)),
            Err(_) => Some(default),
        },
        Err(_) => Some(default),
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

/// How an execution ended, held while a delivery question is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Terminal {
    Succeeded,
    Failed,
    Interrupted,
}

pub(super) struct TurnState {
    /// A prompt is in flight (busy expected/observed; a terminal settles it).
    pub(super) active: bool,
    /// A terminal belongs to this prompt only after its busy transition, or
    /// after we explicitly abort it. A trailing terminal from the previous
    /// busy period must not settle a just-submitted prompt.
    pub(super) idle_ready: bool,
    /// Bus events about our session seen since the prompt was posted.
    pub(super) saw_activity: bool,
    /// Renderable content (text/reasoning/tool) seen this turn.
    pub(super) saw_content: bool,
    /// Error text to fold into Done.
    pub(super) error: Option<String>,
    /// The execution itself failed: always Errored, even after content.
    pub(super) failed: bool,
    /// A provider-side interrupt reason other than a user Stop.
    pub(super) interrupt_reason: Option<String>,
    /// Last provider assistant message in this root turn. OpenCode's native
    /// fork endpoint uses message ids as history checkpoints.
    pub(super) provider_checkpoint_id: Option<String>,
    /// Deadline for the first session-scoped event after the prompt.
    pub(super) stall_deadline: Option<tokio::time::Instant>,
    /// Last bus traffic of this run (post-activity stall).
    pub(super) last_activity: tokio::time::Instant,
    /// The inbox id this turn waits for: its prompt's caller id, or the
    /// first foreign enqueue adopted by a command turn.
    pub(super) inbox_id: Option<String>,
    /// LUNA's message id for a follow-up turn's `Steered` boundary.
    pub(super) luna_message_id: Option<String>,
    /// A follow-up opens its segment (`Steered`) on its own delivery.
    pub(super) rotate_on_delivery: bool,
    /// A command turn adopts the first foreign enqueue as its anchor.
    pub(super) adopt_inbox: bool,
    pub(super) enqueued: bool,
    pub(super) delivered: bool,
    /// Admission (HTTP 200) time, for the no-evidence readback.
    pub(super) admitted_at: Option<tokio::time::Instant>,
    pub(super) admission_checked: bool,
    /// Unix ms at the post (stale `time.idle` detection).
    pub(super) started_ms: i64,
    /// A terminal waiting for this turn's delivery question to resolve.
    pub(super) undelivered: Option<(Terminal, tokio::time::Instant)>,
}

impl TurnState {
    pub(super) fn begin(stall: Option<Duration>) -> Self {
        let now = tokio::time::Instant::now();
        Self {
            active: true,
            idle_ready: false,
            saw_activity: false,
            saw_content: false,
            error: None,
            failed: false,
            interrupt_reason: None,
            provider_checkpoint_id: None,
            stall_deadline: stall.map(|d| now + d),
            last_activity: now,
            inbox_id: None,
            luna_message_id: None,
            rotate_on_delivery: false,
            adopt_inbox: false,
            enqueued: false,
            delivered: false,
            admitted_at: None,
            admission_checked: false,
            started_ms: unix_ms(),
            undelivered: None,
        }
    }

    pub(super) fn note_activity(&mut self) {
        self.saw_activity = true;
        self.stall_deadline = None;
    }
}

/// Apply a posted turn's outcome. `Err` = the turn cannot run: refused, or
/// unknown and absent on readback (never resent either way).
#[allow(clippy::too_many_arguments)]
async fn admit(
    outcome: PromptOutcome,
    turn: &mut TurnState,
    id: &str,
    posted_ms: i64,
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
) -> Result<(), String> {
    let evidence = |receipt: &str, detail: Value| {
        receipt_evidence(
            "prompt",
            Some(id),
            json!({ "inboxID": id, "receipt": receipt, "detail": detail }),
        )
    };
    match outcome {
        PromptOutcome::Command => {
            turn.adopt_inbox = true;
            Ok(())
        }
        PromptOutcome::Accepted {
            inbox_id,
            created_ms,
        } => {
            let _ = send(
                event_tx,
                evidence("accepted", json!({ "createdMs": created_ms })),
            )
            .await;
            turn.inbox_id = Some(inbox_id);
            turn.admitted_at = Some(tokio::time::Instant::now());
            // OpenCode answers a known id with the EARLIER admission (no new
            // enqueue follows): check what happened to it right away.
            if created_ms.is_some_and(|created| created < posted_ms - 2_000) {
                turn.admitted_at = Some(tokio::time::Instant::now() - ADMISSION_GRACE);
            }
            Ok(())
        }
        PromptOutcome::Rejected(message) => {
            let _ = send(event_tx, evidence("refused", json!(message))).await;
            Err(message)
        }
        PromptOutcome::Unknown(message) => {
            let readback = check_delivery(server, session_id, dir, id).await;
            let _ = send(
                event_tx,
                evidence(
                    "unknown",
                    json!({ "error": message, "readback": format!("{readback:?}") }),
                ),
            )
            .await;
            match readback {
                DeliveryReadback::Pending | DeliveryReadback::Delivered => {
                    turn.inbox_id = Some(id.to_owned());
                    turn.admitted_at = Some(tokio::time::Instant::now());
                    Ok(())
                }
                DeliveryReadback::Absent => Err(format!(
                    "OpenCode did not record the prompt ({message}); LUNA did not resend it."
                )),
                DeliveryReadback::Unknown(readback) => Err(format!(
                    "OpenCode's answer to the prompt was lost ({message}) and readback failed \
                     ({readback}); delivery is unknown, so LUNA did not resend it."
                )),
            }
        }
    }
}

/// Bounded insertion-ordered id memory.
#[derive(Default)]
struct IdMemory {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl IdMemory {
    fn insert(&mut self, id: &str, cap: usize) -> bool {
        if self.ids.contains(id) {
            return false;
        }
        if self.order.len() >= cap
            && let Some(oldest) = self.order.pop_front()
        {
            self.ids.remove(&oldest);
        }
        self.order.push_back(id.to_owned());
        self.ids.insert(id.to_owned());
        true
    }
}

/// Which deadline woke the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    Abort,
    FirstEvent,
    Activity,
    Undelivered,
    Admission,
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
    let permission_mode = request
        .permission_policy
        .as_ref()
        .map(|policy| policy.effective_mode)
        .unwrap_or(luna_proto::PermissionMode::Ask);

    // ---- session create/resume -------------------------------------------
    let setup = async {
        // The LUNA mode compiles against the session agent's own rules, so
        // the user's configured denies stay final (`permissions`).
        let rule_set = SessionRuleSet::load(&server, dir, permission_mode).await;
        let (session_id, permission_rules) = match &request.resume {
            Some(resume) => {
                // Sessions are durable server-side: resume = reuse the id.
                // A failed lookup (including a transient server failure) is
                // not authorization to discard this conversation's history.
                let info = server.session_info(resume, dir).await?;
                let session_id = info
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(resume)
                    .to_owned();
                let rules =
                    rule_set.rules_for(info.get("agent").and_then(Value::as_str), RuleScope::Root);
                // `session.update` (204): the resumed session takes this
                // run's LUNA permission mode as its v2 ruleset.
                server
                    .patch_json(
                        &format!("/api/session/{session_id}"),
                        dir,
                        &json!({ "permissions": rules }),
                    )
                    .await?;
                (session_id, rules)
            }
            None => {
                let rules = rule_set.rules_for(None, RuleScope::Root);
                (create_session(&server, dir, &rules).await?, rules)
            }
        };
        // Readback before the first prompt: rules OpenCode dropped or
        // changed fail the run instead of running unprotected (OCF-153).
        verify_session_rules(&server, &session_id, dir, &permission_rules).await?;

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
        Ok::<_, HarnessError>((session_id, providers, rule_set, permission_rules))
    };
    let (session_id, providers, rule_set, permission_rules) = tokio::select! {
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

    // Asks and forms of this run (and its subagent sessions). Their prompts
    // close on Stop (child token) and when the run loop ends (drop).
    let mut requests = UserRequests::new(
        session_id.clone(),
        interrupt.child_token(),
        rule_set,
        permission_rules,
    );

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
    let bus_handle = tokio::spawn(bus_task(
        BusConfig {
            base: server.base.clone(),
            auth: server.auth.clone(),
            session_id: session_id.clone(),
            directory: directory.clone(),
        },
        bus_tx,
    ));

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
    let activity_stall = activity_stall_bound();
    // The engine's LUNA user-message id for this first prompt rides as the
    // caller id, exactly like every later steer.
    let first_luna_id = request
        .model_options
        .get(crate::codex::LUNA_CLIENT_USER_MESSAGE_ID_OPTION)
        .and_then(Value::as_str)
        .map(str::to_owned);
    let first_id = luna_prompt_id(&session_id, first_luna_id.as_deref());
    let posted_ms = unix_ms();
    let first_outcome = post_prompt(
        &server,
        &session_id,
        dir,
        &commands,
        &request.prompt,
        TurnSpec {
            attachments: &request.attachments,
            id: &first_id,
            delivery: PromptDelivery::Queue,
        },
        &command_tx,
    )
    .await;
    let mut turn = TurnState::begin(stall);
    turn.started_ms = posted_ms;
    if let Err(message) = admit(
        first_outcome,
        &mut turn,
        &first_id,
        posted_ms,
        &server,
        &session_id,
        dir,
        &event_tx,
    )
    .await
    {
        let _ = send(
            &event_tx,
            AgentEvent::Error {
                message: message.clone(),
            },
        )
        .await;
        let _ = send(
            &event_tx,
            AgentEvent::Done {
                status: DoneStatus::Errored,
                result: None,
                error: Some(message),
                session_id: Some(session_id.clone()),
            },
        )
        .await;
        bus_handle.abort();
        server.shutdown(kill_grace).await;
        return;
    }

    // ---- main loop --------------------------------------------------------
    let mut main_feed = SessionFeed::default();
    let mut children: HashMap<String, ChildRun> = HashMap::new();
    let mut pending_spawns: VecDeque<PendingSpawn> = VecDeque::new();
    // Child sessions created before their spawn chip was seen (id → title).
    let mut unbound_children: HashMap<String, String> = HashMap::new();
    // Ordinary follow-ups (warm routing) wait for the turn boundary and
    // then run as their own turn; explicit steers go native.
    let mut held_followups: VecDeque<crate::SteerMessage> = VecDeque::new();
    let mut pending_steers: VecDeque<PendingSteer> = VecDeque::new();
    let (steer_posted_tx, mut steer_posted_rx) = mpsc::unbounded_channel::<SteerPosted>();
    // The server speaks the inbox (any inbox frame about our session seen):
    // correlation is by delivery, else by execution only.
    let mut inbox_live = false;
    let mut steering_open = true;
    let mut interrupt_requested = false;
    let mut provider_interrupted = false;
    let mut pending_usage: Option<AgentEvent> = None;
    let mut done_sent = false;
    // Permission/form requests awaiting the user (stall exemption) and
    // every request id seen (reconnect dedupe).
    let mut pending_requests: HashMap<String, String> = HashMap::new();
    let mut seen_requests = IdMemory::default();
    // Tool chips in flight (stall exemption).
    let mut tools_in_flight: HashSet<String> = HashSet::new();
    // Root message ids the bus showed (readback window), and inbox ids
    // already recorded as delivered (one receipt per input).
    let mut known_messages = IdMemory::default();
    let mut delivered_inbox = IdMemory::default();
    if let Some(id) = &turn.inbox_id {
        known_messages.insert(id, MAX_KNOWN_MESSAGES);
    }
    // Readback-synthesized payloads, processed before further bus traffic.
    let mut injected: VecDeque<BusMsg> = VecDeque::new();
    let mut provider_turns: u64 = 0;

    // Post-abort grace: the abort endpoint promised a terminal; if it never
    // arrives the run hard-stops. Unlike the stall bound this is NOT
    // disarmed by activity — only a terminal ends an abort.
    let mut abort_deadline: Option<tokio::time::Instant> = None;

    // Turn settlement, shared by the terminal frames, the delivery
    // readback and the reconnect resync (a macro so `break`/`continue` act
    // on the caller's loop): emit held usage, then Interrupted / the next
    // held follow-up / AssistantMessageCompleted + Done.
    macro_rules! settle_idle {
        ($label:lifetime) => {{
            if !turn.active {
                continue $label;
            }
            turn.active = false;
            turn.undelivered = None;
            tools_in_flight.clear();
            if let Some(usage) = pending_usage.take()
                && !send(&event_tx, usage).await
            {
                break $label;
            }
            if interrupt_requested || provider_interrupted {
                let reason = turn
                    .interrupt_reason
                    .take()
                    .and_then(|reason| interrupt_reason_message(&reason));
                if let Some(reason) = &reason {
                    let _ = send(&event_tx, AgentEvent::Error { message: reason.clone() }).await;
                }
                settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                let _ = send(&event_tx, AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: reason,
                    session_id: Some(session_id.clone()),
                }).await;
                done_sent = true;
                break $label;
            }
            let errored = turn.failed || (turn.error.is_some() && !turn.saw_content);
            if !errored && let Some(next) = held_followups.pop_front() {
                let id = luna_prompt_id(&session_id, next.message_id.as_deref());
                let posted_ms = unix_ms();
                let outcome = post_prompt(
                    &server,
                    &session_id,
                    dir,
                    &commands,
                    &next.prompt,
                    TurnSpec {
                        attachments: &next.attachments,
                        id: &id,
                        delivery: PromptDelivery::Queue,
                    },
                    &command_tx,
                )
                .await;
                let rotate_now = !inbox_live || outcome == PromptOutcome::Command;
                let mut next_turn = TurnState::begin(stall);
                next_turn.started_ms = posted_ms;
                next_turn.provider_checkpoint_id = turn.provider_checkpoint_id.clone();
                next_turn.luna_message_id = next.message_id.clone();
                match admit(outcome, &mut next_turn, &id, posted_ms, &server, &session_id, dir, &event_tx).await {
                    Ok(()) => {
                        turn = next_turn;
                        if let Some(id) = &turn.inbox_id {
                            known_messages.insert(id, MAX_KNOWN_MESSAGES);
                        }
                        if rotate_now {
                            // No inbox evidence to wait for: the boundary
                            // is the post itself (execution correlation).
                            let (prev, next_id) = rotate(&mut assistant_message_id);
                            if !send(&event_tx, AgentEvent::Steered {
                                user_message_id: next.message_id.clone(),
                                assistant_message_id: Some(prev),
                                next_assistant_message_id: Some(next_id),
                            }).await {
                                break $label;
                            }
                        } else {
                            turn.rotate_on_delivery = true;
                        }
                        continue $label;
                    }
                    Err(message) => {
                        // The finished turn still settles as it ended; the
                        // follow-up stays unconfirmed for the engine.
                        let _ = send(&event_tx, AgentEvent::Error { message }).await;
                    }
                }
            }
            let (prev, _next) = rotate(&mut assistant_message_id);
            if !send(&event_tx, AgentEvent::AssistantMessageCompleted {
                assistant_message_id: prev,
            }).await {
                break $label;
            }
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
            break $label;
        }};
    }

    // A terminal for our session: settle, or hold it while this turn's own
    // delivery (or a native steer's) is still open.
    macro_rules! on_terminal {
        ($label:lifetime, $terminal:expr) => {{
            let terminal: Terminal = $terminal;
            tools_in_flight.clear();
            let open = inbox_live
                && !interrupt_requested
                && ((turn.inbox_id.is_some() && !turn.delivered) || !pending_steers.is_empty());
            if open && turn.active {
                turn.undelivered = Some((terminal, tokio::time::Instant::now() + UNDELIVERED_GRACE));
                continue $label;
            }
            match terminal {
                Terminal::Interrupted => provider_interrupted = true,
                Terminal::Failed => turn.failed = true,
                Terminal::Succeeded => {}
            }
            settle_idle!($label);
        }};
    }

    // One bus message (live or readback-injected).
    macro_rules! handle_bus_msg {
        ($label:lifetime, $msg:expr) => {{
            let msg: BusMsg = $msg;
            match msg {
                BusMsg::Connected => {
                    // Asks and forms raised before the bus attached never
                    // reached it: show them, close ones settled meanwhile.
                    resync_pending_requests(Resync {
                        server: &server,
                        dir,
                        event_tx: &event_tx,
                        request_input: &request_input,
                        auto_approve: request.auto_approve,
                        permission_policy: &permission_policy,
                        requests: &requests,
                        children: &children,
                        unbound_children: &unbound_children,
                        turn_active: turn.active,
                    })
                    .await;
                }
                BusMsg::Reconnected { generation, gap } => {
                    if !send(&event_tx, receipt_evidence(
                        "bus.reconnected",
                        None,
                        json!({ "generation": generation, "replay": gap.describe() }),
                    )).await {
                        break $label;
                    }
                    // Asks and forms raised during the gap never reached
                    // the bus: show them, close ones settled meanwhile
                    // (OC-04/OC-05). The frame re-injection below is then
                    // deduplicated by request id (`UserRequests::known`).
                    resync_pending_requests(Resync {
                        server: &server,
                        dir,
                        event_tx: &event_tx,
                        request_input: &request_input,
                        auto_approve: request.auto_approve,
                        permission_policy: &permission_policy,
                        requests: &requests,
                        children: &children,
                        unbound_children: &unbound_children,
                        turn_active: turn.active,
                    })
                    .await;
                    if gap.needs_readback() && turn.active {
                        let anchor = turn.inbox_id.clone();
                        match readback_frames(&server, &session_id, dir, anchor.as_deref(), &known_messages.ids).await {
                            Ok(Some(frames)) => {
                                let mut tools = HashMap::new();
                                let count = frames.len();
                                for frame in frames {
                                    for mut payload in normalize_v2_frame(frame, &mut tools) {
                                        payload["replayed"] = json!(true);
                                        injected.push_back(BusMsg::Event(payload));
                                    }
                                }
                                let _ = send(&event_tx, receipt_evidence(
                                    "readback.messages",
                                    None,
                                    json!({ "frames": count }),
                                )).await;
                            }
                            Ok(None) => {
                                let _ = send(&event_tx, receipt_evidence(
                                    "readback.messages",
                                    None,
                                    json!({ "frames": 0, "anchor": "not found" }),
                                )).await;
                            }
                            Err(error) => {
                                let _ = send(&event_tx, receipt_evidence(
                                    "readback.messages",
                                    None,
                                    json!({ "error": error.to_string() }),
                                )).await;
                            }
                        }
                    }
                    // Pending asks the gap may have swallowed: re-shown once.
                    for pending in pending_request_frames(&server, dir).await {
                        if !seen_requests.insert(&pending.id, MAX_SEEN_REQUESTS) {
                            continue;
                        }
                        pending_requests.insert(pending.id.clone(), pending.session.clone());
                        let mut tools = HashMap::new();
                        for payload in normalize_v2_frame(pending.frame, &mut tools) {
                            injected.push_back(BusMsg::Event(payload));
                        }
                    }
                    // After the replayed frames: settle a turn whose
                    // terminal the gap swallowed, from the session record.
                    injected.push_back(BusMsg::Event(json!({ "type": "luna.resync" })));
                }
                BusMsg::Request { id, session, pending } => {
                    seen_requests.insert(&id, MAX_SEEN_REQUESTS);
                    // Only this run's sessions hold the turn up.
                    let ours = session == session_id
                        || children.contains_key(&session)
                        || unbound_children.contains_key(&session);
                    if pending && ours {
                        pending_requests.insert(id, session);
                    } else if !pending {
                        pending_requests.remove(&id);
                    }
                    turn.last_activity = tokio::time::Instant::now();
                }
                BusMsg::Disconnected => {
                    let crashed = match server.child.as_mut() {
                        Some(child) => child.try_wait().ok().flatten(),
                        None => None,
                    };
                    let message = crate::crash_message(
                        "opencode serve",
                        crashed,
                        &server.stderr_tail,
                    );
                    // A dead server lost the running execution: the turn
                    // ends interrupted, with the reason shown.
                    let lost = crashed.is_some();
                    let message = if lost && turn.active {
                        format!("{message} The running OpenCode turn was lost and is interrupted.")
                    } else {
                        message
                    };
                    if turn.active {
                        let _ = send(&event_tx, AgentEvent::Error { message: message.clone() }).await;
                    }
                    settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                    let _ = send(&event_tx, AgentEvent::Done {
                        status: if interrupt_requested || lost {
                            DoneStatus::Interrupted
                        } else {
                            DoneStatus::Errored
                        },
                        result: None,
                        error: (!interrupt_requested).then_some(message),
                        session_id: Some(session_id.clone()),
                    }).await;
                    done_sent = true;
                    break $label;
                }
                BusMsg::CommandFailed(message) => {
                    // A command that never started its execution would
                    // otherwise stall until the watchdog; one that did
                    // start settles from its own terminal frame.
                    if !send(&event_tx, AgentEvent::Error { message: message.clone() }).await {
                        break $label;
                    }
                    if turn.active && !turn.saw_activity {
                        turn.error = Some(message);
                        settle_idle!($label);
                    }
                }
                BusMsg::Event(event) => {
                    let kind = event.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
                    let props = event.get("properties").cloned().unwrap_or(Value::Null);
                    let ours = props.get("sessionID").and_then(Value::as_str) == Some(session_id.as_str());
                    turn.last_activity = tokio::time::Instant::now();
                    match kind.as_str() {
                        "luna.resync" => {
                            if !turn.active || turn.undelivered.is_some() {
                                continue $label;
                            }
                            match session_outcome(&server, &session_id, dir, turn.started_ms).await {
                                SessionOutcome::Running | SessionOutcome::Unknown(_) => {}
                                SessionOutcome::Succeeded => on_terminal!($label, Terminal::Succeeded),
                                SessionOutcome::Failed => {
                                    turn.error.get_or_insert_with(|| {
                                        "OpenCode recorded the turn as failed while LUNA's event stream was reconnecting".into()
                                    });
                                    turn.failed = true;
                                    settle_idle!($label);
                                }
                                SessionOutcome::Interrupted => {
                                    turn.interrupt_reason = Some("unknown".into());
                                    provider_interrupted = true;
                                    settle_idle!($label);
                                }
                                SessionOutcome::Lost => {
                                    // Not lost while the prompt still waits
                                    // in the inbox (its execution may not
                                    // have started yet).
                                    let waiting = match (&turn.inbox_id, turn.delivered) {
                                        (Some(id), false) => matches!(
                                            check_delivery(&server, &session_id, dir, id).await,
                                            DeliveryReadback::Pending
                                        ),
                                        _ => false,
                                    };
                                    if !waiting {
                                        turn.interrupt_reason = Some("lost".into());
                                        provider_interrupted = true;
                                        settle_idle!($label);
                                    }
                                }
                            }
                            continue $label;
                        }
                        "inbox.enqueued" | "inbox.delivered" | "inbox.cancelled" | "inbox.delivery" if ours => {
                            inbox_live = true;
                            let inbox_id = props.get("inboxID").and_then(Value::as_str).unwrap_or_default().to_owned();
                            known_messages.insert(&inbox_id, MAX_KNOWN_MESSAGES);
                            if kind == "inbox.delivered" && !delivered_inbox.insert(&inbox_id, MAX_KNOWN_MESSAGES) {
                                // Already recorded (a readback window or a
                                // duplicate frame): one receipt per input.
                                continue $label;
                            }
                            let steer_ix = pending_steers.iter().position(|s| s.inbox_id == inbox_id);
                            let own = turn.inbox_id.as_deref() == Some(inbox_id.as_str());
                            if turn.active && !own && steer_ix.is_none() && turn.adopt_inbox
                                && turn.inbox_id.is_none() && kind == "inbox.enqueued"
                                && props.get("itemType").and_then(Value::as_str) == Some("user")
                            {
                                // A command turn's prompt: adopt it.
                                turn.inbox_id = Some(inbox_id.clone());
                                turn.enqueued = true;
                                turn.note_activity();
                                let _ = send(&event_tx, receipt_evidence("inbox.enqueued", Some(&inbox_id),
                                    json!({ "receipt": "accepted", "owner": "command", "delivery": props.get("delivery") }))).await;
                                continue $label;
                            }
                            let owner = if own { "turn" } else if steer_ix.is_some() { "steer" } else { "foreign" };
                            let receipt = match kind.as_str() {
                                "inbox.enqueued" => "accepted",
                                "inbox.delivered" => "recorded",
                                "inbox.cancelled" => "cancelled",
                                _ => "delivery-changed",
                            };
                            if !send(&event_tx, receipt_evidence(&kind, Some(&inbox_id), json!({
                                "receipt": receipt,
                                "owner": owner,
                                "delivery": props.get("delivery"),
                                "itemType": props.get("itemType"),
                                "replayed": event.get("replayed").is_some(),
                            }))).await {
                                break $label;
                            }
                            if turn.active {
                                turn.note_activity();
                            }
                            match (kind.as_str(), own, steer_ix) {
                                ("inbox.enqueued", true, _) => turn.enqueued = true,
                                ("inbox.delivered", true, _) => {
                                    turn.enqueued = true;
                                    if !turn.delivered {
                                        turn.delivered = true;
                                        // The delivery is the turn's start:
                                        // a terminal before it was not ours.
                                        turn.idle_ready = true;
                                        if turn.rotate_on_delivery {
                                            turn.rotate_on_delivery = false;
                                            let (prev, next) = rotate(&mut assistant_message_id);
                                            if !send(&event_tx, AgentEvent::Steered {
                                                user_message_id: turn.luna_message_id.clone(),
                                                assistant_message_id: Some(prev),
                                                next_assistant_message_id: Some(next),
                                            }).await {
                                                break $label;
                                            }
                                        }
                                    }
                                    if turn.undelivered.is_some() && pending_steers.is_empty() {
                                        // The held terminal predates this
                                        // delivery: wait for the next one.
                                        turn.undelivered = None;
                                    }
                                }
                                ("inbox.delivered", false, Some(ix)) => {
                                    let steer = pending_steers.remove(ix).expect("indexed above");
                                    // Recorded in native history at a step
                                    // boundary of the running execution.
                                    let (prev, next) = rotate(&mut assistant_message_id);
                                    if !send(&event_tx, AgentEvent::Steered {
                                        user_message_id: steer.luna_id.clone(),
                                        assistant_message_id: Some(prev),
                                        next_assistant_message_id: Some(next),
                                    }).await {
                                        break $label;
                                    }
                                    if turn.undelivered.is_some()
                                        && pending_steers.is_empty()
                                        && (turn.delivered || turn.inbox_id.is_none())
                                    {
                                        // Its execution is the next busy
                                        // period: the held terminal is not
                                        // this turn's end any more.
                                        turn.undelivered = None;
                                    }
                                }
                                ("inbox.cancelled", false, Some(ix)) => {
                                    // Withdrawn natively: no boundary. The
                                    // engine retires the row as unknown /
                                    // no-replay at the turn end.
                                    pending_steers.remove(ix);
                                    if let Some((terminal, _)) = turn.undelivered
                                        && pending_steers.is_empty()
                                        && (turn.delivered || turn.inbox_id.is_none())
                                    {
                                        turn.undelivered = None;
                                        on_terminal!($label, terminal);
                                    }
                                }
                                ("inbox.cancelled", true, _) if turn.active && !turn.delivered => {
                                    // Withdrawn before delivery (by Stop, or
                                    // by another client): nothing will run.
                                    if !interrupt_requested {
                                        turn.interrupt_reason = Some("cancelled".into());
                                    }
                                    provider_interrupted = true;
                                    settle_idle!($label);
                                }
                                ("inbox.delivered", false, None) => {
                                    // Input LUNA did not post (a background
                                    // completion, another client): a
                                    // provider-initiated turn, never a LUNA
                                    // receipt.
                                    provider_turns += 1;
                                    if !send(&event_tx, AgentEvent::ProviderTurnStarted {
                                        thread_id: session_id.clone(),
                                        turn_id: format!("{inbox_id}#{provider_turns}"),
                                    }).await {
                                        break $label;
                                    }
                                }
                                _ => {}
                            }
                            continue $label;
                        }
                        "session.retry" if ours => {
                            if turn.active {
                                turn.note_activity();
                            }
                            if !send(&event_tx, retry_notice(&props)).await {
                                break $label;
                            }
                            continue $label;
                        }
                        "session.finish" if ours => {
                            if let Some(notice) = finish_notice(&props)
                                && !send(&event_tx, notice).await
                            {
                                break $label;
                            }
                            continue $label;
                        }
                        "session.synthetic" => {
                            if !send(&event_tx, receipt_evidence("session.synthetic", None, json!({
                                "sessionID": props.get("sessionID"),
                                "description": props.get("description"),
                            }))).await {
                                break $label;
                            }
                            continue $label;
                        }
                        "session.status" if ours
                            && props.pointer("/status/type").and_then(Value::as_str) == Some("busy") =>
                        {
                            // A new busy period: a held terminal was the
                            // previous one's.
                            turn.undelivered = None;
                        }
                        "message.updated" => {
                            let info = props.get("info").unwrap_or(&Value::Null);
                            if info.get("sessionID").and_then(Value::as_str) == Some(session_id.as_str())
                                && info.get("role").and_then(Value::as_str) == Some("assistant")
                                && let Some(id) = info.get("id").and_then(Value::as_str)
                            {
                                known_messages.insert(id, MAX_KNOWN_MESSAGES);
                            }
                        }
                        "message.part.updated" => {
                            let part = props.get("part").unwrap_or(&Value::Null);
                            if part.get("type").and_then(Value::as_str) == Some("tool")
                                && let Some(id) = part.get("id").and_then(Value::as_str)
                            {
                                match part.pointer("/state/status").and_then(Value::as_str) {
                                    Some("pending" | "running") => {
                                        tools_in_flight.insert(id.to_owned());
                                    }
                                    _ => {
                                        tools_in_flight.remove(id);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
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
                        pending_spawns: &mut pending_spawns,
                        unbound_children: &mut unbound_children,
                        turn: &mut turn,
                        pending_usage: &mut pending_usage,
                        context_windows: &context_windows,
                        requests: &mut requests,
                        bus_tx: &command_tx,
                    }).await;
                    match outcome {
                        BusOutcome::Continue => {}
                        BusOutcome::ConsumerGone => break $label,
                        BusOutcome::TurnIdle => on_terminal!($label, Terminal::Succeeded),
                        BusOutcome::TurnFailed => on_terminal!($label, Terminal::Failed),
                        BusOutcome::TurnInterrupted => on_terminal!($label, Terminal::Interrupted),
                    }
                }
            }
        }};
    }

    'main: loop {
        // The first-event watchdog only arms while a turn awaits its first
        // sign of life; the activity bound arms afterwards, but never while
        // a request waits on the user or a tool runs. Computed by value each
        // iteration so the future doesn't hold a borrow of `turn`.
        let now = tokio::time::Instant::now();
        let mut deadlines: Vec<(tokio::time::Instant, Wake)> = Vec::new();
        if let Some(deadline) = abort_deadline {
            deadlines.push((deadline, Wake::Abort));
        } else if turn.active {
            if !turn.saw_activity
                && let Some(deadline) = turn.stall_deadline
            {
                deadlines.push((deadline, Wake::FirstEvent));
            }
            if turn.saw_activity
                && pending_requests.is_empty()
                && tools_in_flight.is_empty()
                && turn.undelivered.is_none()
                && let Some(bound) = activity_stall
            {
                deadlines.push((turn.last_activity + bound, Wake::Activity));
            }
            if let Some((_, deadline)) = turn.undelivered {
                deadlines.push((deadline, Wake::Undelivered));
            }
            if !turn.admission_checked
                && !turn.enqueued
                && !turn.delivered
                && !turn.idle_ready
                && let Some(admitted) = turn.admitted_at
            {
                deadlines.push((admitted + ADMISSION_GRACE, Wake::Admission));
            }
        }
        let next = deadlines.into_iter().min_by_key(|(deadline, _)| *deadline);
        let wake_sleep = async {
            match next {
                Some((deadline, _)) => tokio::time::sleep_until(deadline.max(now)).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;

            _ = interrupt.cancelled(), if !interrupt_requested => {
                interrupt_requested = true;
                // Stop clears pending input: queued follow-ups never run
                // (the engine pauses the queue after Stop).
                held_followups.clear();
                if turn.active {
                    turn.idle_ready = true;
                    turn.undelivered = None;
                    // Interrupt WITHOUT `resume`: OpenCode would otherwise
                    // promote undelivered steers into a successor.
                    let abort = tokio::time::timeout(
                        Duration::from_secs(5),
                        server.interrupt_session(&session_id, dir),
                    )
                    .await;
                    // Then withdraw every undelivered native input, so a
                    // later prompt on this session never drains it.
                    let mut withdraw: Vec<String> =
                        pending_steers.iter().map(|steer| steer.inbox_id.clone()).collect();
                    if !turn.delivered && let Some(id) = &turn.inbox_id {
                        withdraw.push(id.clone());
                    }
                    for inbox_id in withdraw {
                        let result = cancel_inbox(&server, &session_id, dir, &inbox_id).await;
                        let _ = send(&event_tx, receipt_evidence("inbox.withdraw", Some(&inbox_id), json!({
                            "reason": "stop",
                            "result": match &result { Ok(()) => json!("requested"), Err(error) => json!(error) },
                        }))).await;
                    }
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
                            let refusal = if !turn.active || interrupt_requested {
                                Some("no OpenCode turn is running to steer")
                            } else if known_command(&steer.prompt, &commands).is_some() {
                                Some("OpenCode runs a slash command only as its own turn")
                            } else {
                                None
                            };
                            if let Some(refusal) = refusal {
                                if let Some(tx) = steer.acceptance {
                                    let _ = tx.send(crate::SteerAcceptance::Rejected(refusal.into()));
                                }
                                continue 'main;
                            }
                            // Native steer: admitted into the running
                            // execution's inbox, incorporated at its next
                            // step boundary (`inbox.delivered`).
                            let inbox_id = luna_prompt_id(&session_id, steer.message_id.as_deref());
                            known_messages.insert(&inbox_id, MAX_KNOWN_MESSAGES);
                            pending_steers.push_back(PendingSteer {
                                inbox_id: inbox_id.clone(),
                                luna_id: steer.message_id.clone(),
                                accepted: false,
                            });
                            let poster = server.detached();
                            let session = session_id.clone();
                            let dir_owned = directory.clone();
                            let posted = steer_posted_tx.clone();
                            tokio::spawn(async move {
                                let body = prompt_body_v2(
                                    &steer.prompt,
                                    &steer.attachments,
                                    &inbox_id,
                                    PromptDelivery::Steer,
                                );
                                let path = format!("/api/session/{session}/prompt");
                                let outcome = classify_prompt_response(
                                    &path,
                                    &inbox_id,
                                    poster.post_json_raw(&path, dir_owned.as_deref(), &body).await,
                                );
                                if let Some(tx) = steer.acceptance {
                                    let _ = tx.send(match &outcome {
                                        PromptOutcome::Accepted { .. } => crate::SteerAcceptance::Accepted,
                                        PromptOutcome::Rejected(message) => {
                                            crate::SteerAcceptance::Rejected(message.clone())
                                        }
                                        PromptOutcome::Unknown(message) => {
                                            crate::SteerAcceptance::Unknown(message.clone())
                                        }
                                        PromptOutcome::Command => crate::SteerAcceptance::Unknown(
                                            "unexpected command routing for a steer".into(),
                                        ),
                                    });
                                }
                                let _ = posted.send(SteerPosted { inbox_id, outcome });
                            });
                            continue 'main;
                        }
                        // Ordinary warm routing: its own turn, posted at this
                        // turn's boundary. (A turn is always active here: the
                        // loop ends with the Done that settles the last one.)
                        held_followups.push_back(steer);
                    }
                    None => steering_open = false,
                }
            }

            posted = steer_posted_rx.recv() => {
                let Some(SteerPosted { inbox_id, outcome }) = posted else { continue 'main };
                let receipt = match &outcome {
                    PromptOutcome::Accepted { .. } => "accepted",
                    PromptOutcome::Rejected(_) => "refused",
                    _ => "unknown",
                };
                let detail = match &outcome {
                    PromptOutcome::Rejected(message) | PromptOutcome::Unknown(message) => json!(message),
                    _ => Value::Null,
                };
                if !send(&event_tx, receipt_evidence("steer", Some(&inbox_id), json!({
                    "inboxID": inbox_id,
                    "receipt": receipt,
                    "detail": detail,
                }))).await {
                    break 'main;
                }
                match outcome {
                    PromptOutcome::Accepted { .. } => {
                        if let Some(steer) = pending_steers.iter_mut().find(|s| s.inbox_id == inbox_id) {
                            steer.accepted = true;
                        }
                    }
                    PromptOutcome::Rejected(_) => {
                        // Nothing was recorded: no boundary will follow.
                        pending_steers.retain(|s| s.inbox_id != inbox_id);
                        if let Some((terminal, _)) = turn.undelivered
                            && pending_steers.is_empty()
                            && (turn.delivered || turn.inbox_id.is_none())
                        {
                            turn.undelivered = None;
                            on_terminal!('main, terminal);
                        }
                    }
                    // Unknown: it may still show up as delivered (then it
                    // is recorded late); the turn-end readback decides.
                    _ => {}
                }
            }

            // Queued bus traffic (readback-injected first) drains before any
            // deadline is judged: a busy or delivery frame already waiting
            // here must win over a grace that just expired.
            msg = next_bus_msg(&mut injected, &mut bus_rx) => {
                let Some(msg) = msg else { break 'main };
                handle_bus_msg!('main, msg);
            }

            _ = wake_sleep => {
                let Some((_, wake)) = next else { continue 'main };
                match wake {
                    Wake::Abort => {
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
                    Wake::FirstEvent | Wake::Activity => {
                        let message = if wake == Wake::FirstEvent {
                            format!(
                                "opencode made no progress for {}s after the prompt. {STALL_HINT}",
                                stall.unwrap_or(DEFAULT_STALL_BOUND).as_secs()
                            )
                        } else {
                            format!(
                                "opencode went silent for {}s mid-turn (no output, no running tool, \
                                 nothing awaiting input). {STALL_HINT}",
                                activity_stall.unwrap_or(DEFAULT_ACTIVITY_STALL_BOUND).as_secs()
                            )
                        };
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
                    Wake::Admission => {
                        turn.admission_checked = true;
                        let Some(id) = turn.inbox_id.clone() else { continue 'main };
                        let readback = check_delivery(&server, &session_id, dir, &id).await;
                        let _ = send(&event_tx, receipt_evidence("prompt.readback", Some(&id), json!({
                            "reason": "admitted without enqueue evidence",
                            "readback": format!("{readback:?}"),
                        }))).await;
                        match readback {
                            DeliveryReadback::Delivered => {
                                if !matches!(
                                    session_outcome(&server, &session_id, dir, turn.started_ms).await,
                                    SessionOutcome::Running
                                ) {
                                    // OpenCode reconciled an id it had already
                                    // delivered: nothing new will run.
                                    turn.delivered = true;
                                    turn.error = Some(
                                        "OpenCode had already recorded this message earlier, so it \
                                         was not sent again; its reply is in OpenCode's history \
                                         but was not streamed here."
                                            .into(),
                                    );
                                    turn.failed = true;
                                    settle_idle!('main);
                                }
                            }
                            DeliveryReadback::Absent => {
                                turn.error = Some(
                                    "OpenCode accepted the prompt but no longer has it (cancelled \
                                     before delivery)."
                                        .into(),
                                );
                                turn.failed = true;
                                settle_idle!('main);
                            }
                            DeliveryReadback::Pending | DeliveryReadback::Unknown(_) => {}
                        }
                    }
                    Wake::Undelivered => {
                        let Some((mut terminal, _)) = turn.undelivered.take() else { continue 'main };
                        // The turn's own prompt never got delivered in the
                        // busy period that just ended.
                        if !turn.delivered && let Some(id) = turn.inbox_id.clone() {
                            let readback = check_delivery(&server, &session_id, dir, &id).await;
                            let mut note: Option<String> = None;
                            match &readback {
                                DeliveryReadback::Pending => {
                                    let _ = cancel_inbox(&server, &session_id, dir, &id).await;
                                    note = Some("OpenCode ended the execution before delivering the prompt; LUNA withdrew it from OpenCode's inbox.".into());
                                }
                                DeliveryReadback::Delivered => turn.delivered = true,
                                DeliveryReadback::Absent => {
                                    note = Some("OpenCode has no record of the prompt after the execution ended.".into());
                                }
                                DeliveryReadback::Unknown(error) => {
                                    note = Some(format!("The prompt's delivery is unknown ({error}); LUNA did not resend it."));
                                }
                            }
                            let _ = send(&event_tx, receipt_evidence("prompt.readback", Some(&id), json!({
                                "reason": "terminal before delivery",
                                "readback": format!("{readback:?}"),
                            }))).await;
                            if let Some(note) = note {
                                turn.error.get_or_insert(note);
                                if terminal == Terminal::Succeeded {
                                    terminal = Terminal::Failed;
                                }
                            }
                        }
                        // Native steers the ended execution never delivered.
                        for steer in std::mem::take(&mut pending_steers) {
                            let readback = check_delivery(&server, &session_id, dir, &steer.inbox_id).await;
                            if readback == DeliveryReadback::Pending {
                                let _ = cancel_inbox(&server, &session_id, dir, &steer.inbox_id).await;
                            }
                            if readback == DeliveryReadback::Delivered {
                                let (prev, next) = rotate(&mut assistant_message_id);
                                let _ = send(&event_tx, AgentEvent::Steered {
                                    user_message_id: steer.luna_id.clone(),
                                    assistant_message_id: Some(prev),
                                    next_assistant_message_id: Some(next),
                                }).await;
                            }
                            let _ = send(&event_tx, receipt_evidence("steer.readback", Some(&steer.inbox_id), json!({
                                "reason": "terminal before delivery",
                                "readback": format!("{readback:?}"),
                                "withdrawn": readback == DeliveryReadback::Pending,
                            }))).await;
                        }
                        match terminal {
                            Terminal::Interrupted => provider_interrupted = true,
                            Terminal::Failed => turn.failed = true,
                            Terminal::Succeeded => {}
                        }
                        settle_idle!('main);
                    }
                }
            }
        }
    }

    if !done_sent {
        // Consumer went away (stream dropped): nothing to report to.
        tracing::debug!(target: "luna_harness::opencode", "run loop ended without settling");
    }
    // Close every prompt still open; nothing is answered after the turn.
    drop(requests);
    bus_handle.abort();
    server.shutdown(kill_grace).await;
}

/// The next bus message: readback-injected payloads first, then the live
/// bus. Cancel-safe (an injected message is only taken on a ready poll).
async fn next_bus_msg(
    injected: &mut VecDeque<BusMsg>,
    bus_rx: &mut mpsc::Receiver<BusMsg>,
) -> Option<BusMsg> {
    if let Some(msg) = injected.pop_front() {
        return Some(msg);
    }
    bus_rx.recv().await
}
