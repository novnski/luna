//! Live authoring runtimes: at most one provider turn per session, driven
//! directly on the harness (never through the chat `SessionsEngine`, so no
//! `SessionDoc`, run journal, recovery record or activity history exists for
//! a discussion).
//!
//! A turn is one harness run: its steering mailbox is closed up front so the
//! provider ends input after this message, the stream is read to its `Done`
//! and then dropped, which ends the provider process and its process-scoped
//! `luna` MCP endpoint. The next message starts a new run that resumes the
//! recorded provider session. Close/Discard refuse new tool calls at once,
//! let one already running finish (its receipt is kept), interrupt the turn
//! and abort it if the provider does not stop in time.
//!
//! State shared by every `EngineRpc` of one engine lives beside its
//! `BoardHub` (keyed by the hub's allocation) so the foundation hub type
//! stays unchanged.
use super::*;
use luna_harness::{HarnessError, LunaToolExecutor, LunaToolInvocation, RunControls};
use luna_proto::{AgentEvent, DoneStatus, UserInputAnswer, UserInputQuestion};
use std::sync::{Mutex, MutexGuard, PoisonError, Weak};
use tokio::time::Instant;

/// Close waits this long for a board tool call that is already running.
const TOOL_WAIT: Duration = Duration::from_secs(20);
/// Then this long for the interrupted provider to end its turn.
const STOP_WAIT: Duration = Duration::from_secs(25);
const WATCH_POLL: Duration = Duration::from_secs(5);
/// Streaming deltas are coalesced into one frame per this interval.
const WATCH_COALESCE: Duration = Duration::from_millis(60);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

type HubRuntimes = (Weak<super::super::BoardHubInner>, Arc<AuthoringRuntimes>);

static TABLE: Mutex<Vec<HubRuntimes>> = Mutex::new(Vec::new());

/// The authoring runtimes of the engine that owns `hub`.
pub(crate) fn runtimes(hub: &BoardHub) -> Arc<AuthoringRuntimes> {
    let mut table = lock(&TABLE);
    table.retain(|(owner, _)| owner.strong_count() > 0);
    if let Some((_, runtimes)) = table
        .iter()
        .find(|(owner, _)| std::ptr::eq(owner.as_ptr(), Arc::as_ptr(&hub.0)))
    {
        return runtimes.clone();
    }
    let runtimes = Arc::new(AuthoringRuntimes::default());
    table.push((Arc::downgrade(&hub.0), runtimes.clone()));
    runtimes
}

/// Called once per `EngineRpc` the engine hands out: a running turn reaches
/// the board service through the newest live one (held weakly, so a turn
/// never keeps a dropped engine alive).
pub(crate) fn register_service(rpc: &Arc<EngineRpc>) {
    let runtimes = runtimes(rpc.board_hub());
    let mut services = lock(&runtimes.services);
    services.retain(|service| service.strong_count() > 0);
    services.push(Arc::downgrade(rpc));
}

/// Engine shutdown: stop every live discussion turn (never left working).
pub(crate) async fn shutdown_all(hub: &BoardHub) {
    let runtimes = runtimes(hub);
    let live: Vec<String> = lock(&runtimes.turns).keys().cloned().collect();
    for session_id in live {
        runtimes.stop(&session_id).await;
    }
}

/// Everything one turn needs, resolved before the user's message is recorded.
pub(crate) struct PreparedRun {
    pub(crate) harness: Arc<dyn luna_harness::Harness>,
    pub(crate) request: RunRequest,
    pub(crate) contract: LunaAgentRuntimeContract,
}

struct LiveTurn {
    turn_id: String,
    assistant_id: String,
    started_at_ms: i64,
    interrupt: CancellationToken,
    /// Close/Discard began: new tool calls are refused.
    closing: bool,
    /// Board tool calls currently executing.
    tools: usize,
    /// The streaming assistant entry (persisted when the turn ends).
    parts: Vec<MessagePart>,
    task: Option<tokio::task::AbortHandle>,
}

pub(crate) struct AuthoringRuntimes {
    turns: Mutex<HashMap<String, LiveTurn>>,
    gates: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// Serializes session-row read-modify-writes and entry numbering.
    writes: Mutex<()>,
    /// Bumped on every live or committed authoring change (watch wake-up).
    changed: watch::Sender<u64>,
    services: Mutex<Vec<Weak<EngineRpc>>>,
}

impl Default for AuthoringRuntimes {
    fn default() -> Self {
        Self {
            turns: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            writes: Mutex::new(()),
            changed: watch::channel(0).0,
            services: Mutex::new(Vec::new()),
        }
    }
}

/// One running board tool call; Close waits for it before interrupting.
pub(crate) struct ToolCallGuard {
    runtimes: Arc<AuthoringRuntimes>,
    session_id: String,
    turn_id: String,
}

impl Drop for ToolCallGuard {
    fn drop(&mut self) {
        if let Some(turn) = lock(&self.runtimes.turns).get_mut(&self.session_id)
            && turn.turn_id == self.turn_id
        {
            turn.tools = turn.tools.saturating_sub(1);
        }
        self.runtimes.notify();
    }
}

impl AuthoringRuntimes {
    /// Serialize lifecycle operations (start/send/close/discard) on one
    /// session. Tool calls never take it, so Close can wait for them.
    pub(crate) async fn gate(&self, session_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let gate = {
            let mut gates = lock(&self.gates);
            gates.retain(|_, gate| gate.strong_count() > 0);
            match gates.get(session_id).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(tokio::sync::Mutex::new(()));
                    gates.insert(session_id.into(), Arc::downgrade(&gate));
                    gate
                }
            }
        };
        gate.lock_owned().await
    }

    pub(crate) fn write_guard(&self) -> MutexGuard<'_, ()> {
        lock(&self.writes)
    }

    pub(crate) fn notify(&self) {
        self.changed
            .send_modify(|value| *value = value.wrapping_add(1));
    }

    pub(crate) fn service(&self) -> Option<Arc<EngineRpc>> {
        lock(&self.services).iter().rev().find_map(Weak::upgrade)
    }

    pub(crate) fn is_working(&self, session_id: &str) -> bool {
        lock(&self.turns).contains_key(session_id)
    }

    /// The running turn's assistant entry as it streams.
    pub(crate) fn live_entry(
        &self,
        session_id: &str,
        device_id: &str,
    ) -> Option<SessionMessageEntry> {
        let turns = lock(&self.turns);
        let turn = turns.get(session_id)?;
        Some(SessionMessageEntry {
            id: turn.assistant_id.clone(),
            role: MessageRole::Assistant,
            parts: turn.parts.clone(),
            created_at: turn.started_at_ms,
            device_id: device_id.into(),
            status: Some(MessageStatus::Streaming),
            continuation_of: None,
        })
    }

    pub(crate) fn push_part(&self, session_id: &str, turn_id: &str, part: MessagePart) {
        if let Some(turn) = lock(&self.turns).get_mut(session_id)
            && turn.turn_id == turn_id
        {
            turn.parts.push(part);
        }
        self.notify();
    }

    fn fold(&self, session_id: &str, turn_id: &str, event: &AgentEvent) {
        if let Some(turn) = lock(&self.turns).get_mut(session_id)
            && turn.turn_id == turn_id
        {
            luna_doc::fold_event_into_parts(&mut turn.parts, event);
        }
        self.notify();
    }

    /// Admit one board tool call from the live turn `turn_id`.
    pub(crate) fn enter_tool(
        self: &Arc<Self>,
        session_id: &str,
        turn_id: &str,
    ) -> BoardResult<ToolCallGuard> {
        let mut turns = lock(&self.turns);
        match turns.get_mut(session_id) {
            Some(turn)
                if turn.turn_id == turn_id && !turn.closing && !turn.interrupt.is_cancelled() =>
            {
                turn.tools += 1;
                Ok(ToolCallGuard {
                    runtimes: self.clone(),
                    session_id: session_id.into(),
                    turn_id: turn_id.into(),
                })
            }
            _ => Err(BoardError::Unavailable {
                reason: "This Board discussion is stopping or closed; the call was not run.".into(),
            }),
        }
    }

    async fn wait_until(
        &self,
        timeout: Duration,
        done: impl Fn(&HashMap<String, LiveTurn>) -> bool,
    ) -> bool {
        let mut changes = self.changed.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            if done(&lock(&self.turns)) {
                return true;
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() {
                    return done(&lock(&self.turns));
                },
                _ = tokio::time::sleep_until(deadline) => return done(&lock(&self.turns)),
            }
        }
    }

    /// Abort a turn whose provider did not stop in time (dropping its stream
    /// ends the provider process and its endpoint).
    fn abort(&self, session_id: &str) {
        if let Some(turn) = lock(&self.turns).remove(session_id)
            && let Some(task) = turn.task
        {
            task.abort();
        }
        self.notify();
    }

    /// Stop the current turn without closing the session.
    pub(crate) async fn interrupt(&self, session_id: &str) {
        let token = lock(&self.turns)
            .get(session_id)
            .map(|turn| turn.interrupt.clone());
        let Some(token) = token else {
            return;
        };
        token.cancel();
        if !self
            .wait_until(STOP_WAIT, |turns| !turns.contains_key(session_id))
            .await
        {
            self.abort(session_id);
        }
    }

    /// Close/Discard: refuse new tool calls, let a running one finish, then
    /// interrupt and (if needed) abort. Returns once no turn is live.
    pub(crate) async fn stop(&self, session_id: &str) {
        let live = match lock(&self.turns).get_mut(session_id) {
            Some(turn) => {
                turn.closing = true;
                true
            }
            None => false,
        };
        if !live {
            return;
        }
        self.notify();
        self.wait_until(TOOL_WAIT, |turns| {
            turns.get(session_id).is_none_or(|turn| turn.tools == 0)
        })
        .await;
        self.interrupt(session_id).await;
    }
}

/// Register the turn and spawn its driver.
pub(crate) fn start_turn(
    runtimes: Arc<AuthoringRuntimes>,
    session_id: String,
    prepared: PreparedRun,
) -> BoardResult<()> {
    let turn_id = format!("turn-{}", uuid::Uuid::new_v4().simple());
    let interrupt = CancellationToken::new();
    {
        let mut turns = lock(&runtimes.turns);
        if turns.contains_key(&session_id) {
            return Err(BoardError::Busy {
                reason: "The assistant is still answering. Wait for it or press Stop.".into(),
            });
        }
        turns.insert(
            session_id.clone(),
            LiveTurn {
                turn_id: turn_id.clone(),
                assistant_id: format!("{turn_id}-assistant"),
                started_at_ms: crate::now_ms(),
                interrupt: interrupt.clone(),
                closing: false,
                tools: 0,
                parts: Vec::new(),
                task: None,
            },
        );
    }
    let task = tokio::spawn(drive(
        runtimes.clone(),
        session_id.clone(),
        turn_id.clone(),
        prepared,
        interrupt,
    ));
    if let Some(turn) = lock(&runtimes.turns).get_mut(&session_id)
        && turn.turn_id == turn_id
    {
        turn.task = Some(task.abort_handle());
    }
    runtimes.notify();
    Ok(())
}

/// A temporary discussion cannot approve actions or answer blocking
/// prompts: every provider request is declined at once (the least
/// privileged answer) and the decline is shown in the transcript.
fn decline_inputs(
    runtimes: Arc<AuthoringRuntimes>,
    session_id: String,
    turn_id: String,
) -> Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
> {
    Box::new(move |questions: Vec<UserInputQuestion>| {
        let (answer, receiver) = tokio::sync::oneshot::channel();
        let answers = questions
            .iter()
            .map(|question| UserInputAnswer {
                question_id: question.id.clone(),
                labels: vec![decline_label(question)],
            })
            .collect();
        for question in &questions {
            let what = if question.header.trim().is_empty() {
                question.question.trim()
            } else {
                question.header.trim()
            };
            runtimes.push_part(
                &session_id,
                &turn_id,
                MessagePart::Error {
                    id: format!("declined-{}", uuid::Uuid::new_v4().simple()),
                    message: format!(
                        "Declined “{}”: a temporary Board discussion cannot approve actions or answer prompts. Make changes from a normal chat.",
                        what.chars().take(120).collect::<String>()
                    ),
                },
            );
        }
        let _ = answer.send(answers);
        receiver
    })
}

fn decline_label(question: &UserInputQuestion) -> String {
    let lower = |label: &String| label.trim().to_ascii_lowercase();
    if let Some(label) = question.options.iter().find(|label| {
        let label = lower(label);
        label.starts_with("deny")
            || label.starts_with("decline")
            || label.starts_with("reject")
            || label == "no"
    }) {
        return label.clone();
    }
    if question.approval.is_none()
        && let Some(label) = question
            .options
            .iter()
            .find(|label| label.trim_end().ends_with("(Recommended)"))
    {
        return label.clone();
    }
    "Not answered here: ask in the Board discussion instead.".into()
}

fn tool_error(error: &BoardError) -> HarnessError {
    HarnessError::Tool(serde_json::to_string(error).unwrap_or_else(|_| error.message()))
}

/// The engine-owned dispatch for this turn's `luna` tool calls. The caller
/// is bound from the session record inside `authoring_tool_call`.
fn tool_executor(
    runtimes: Arc<AuthoringRuntimes>,
    session_id: String,
    turn_id: String,
    contract: LunaAgentRuntimeContract,
    cancel: CancellationToken,
) -> LunaToolExecutor {
    let contract = Arc::new(contract);
    Arc::new(move |invocation: LunaToolInvocation| {
        let runtimes = runtimes.clone();
        let session_id = session_id.clone();
        let turn_id = turn_id.clone();
        let contract = contract.clone();
        let cancel = cancel.clone();
        Box::pin(async move {
            let Some(rpc) = runtimes.service() else {
                return Err(tool_error(&BoardError::Unavailable {
                    reason: "The engine is shutting down.".into(),
                }));
            };
            rpc.authoring_tool_call(
                &session_id,
                &turn_id,
                &invocation.name,
                invocation.arguments,
                &contract,
                &cancel,
            )
            .await
            .map_err(|error| tool_error(&error))
        }) as futures::future::BoxFuture<'static, Result<Value, HarnessError>>
    })
}

fn record_binding(runtimes: &AuthoringRuntimes, session_id: &str, binding: &str) {
    if let Some(rpc) = runtimes.service()
        && let Err(error) = rpc.authoring_record_binding(session_id, binding)
    {
        tracing::warn!(session_id, %error, "could not record a Board discussion's provider binding");
    }
}

async fn drive(
    runtimes: Arc<AuthoringRuntimes>,
    session_id: String,
    turn_id: String,
    prepared: PreparedRun,
    interrupt: CancellationToken,
) {
    let PreparedRun {
        harness,
        request,
        contract,
    } = prepared;
    let (steer, steering) = tokio::sync::mpsc::channel(1);
    // One turn per run: the provider ends its input after this message.
    drop(steer);
    let controls = RunControls {
        request_input: decline_inputs(runtimes.clone(), session_id.clone(), turn_id.clone()),
        steering,
        interrupt: interrupt.clone(),
        invoke_luna_tool: Some(tool_executor(
            runtimes.clone(),
            session_id.clone(),
            turn_id.clone(),
            contract,
            interrupt.clone(),
        )),
    };
    let mut aborted = false;
    let mut error = None;
    match harness.run(request, controls).await {
        Err(failure) => {
            aborted = true;
            error = Some(failure.to_string());
            runtimes.fold(
                &session_id,
                &turn_id,
                &AgentEvent::Error {
                    message: failure.to_string(),
                },
            );
        }
        Ok(mut stream) => {
            let mut done = false;
            while let Some(item) = stream.next().await {
                let event = match item {
                    Ok(event) => event,
                    Err(failure) => {
                        error = Some(failure.to_string());
                        AgentEvent::Error {
                            message: failure.to_string(),
                        }
                    }
                };
                match &event {
                    AgentEvent::SessionStarted {
                        session_id: binding,
                        ..
                    } => record_binding(&runtimes, &session_id, binding),
                    AgentEvent::Done {
                        status,
                        error: failure,
                        session_id: binding,
                        ..
                    } => {
                        if let Some(binding) = binding {
                            record_binding(&runtimes, &session_id, binding);
                        }
                        aborted = *status != DoneStatus::Completed;
                        error = failure.clone().or(match status {
                            DoneStatus::Completed => None,
                            DoneStatus::Interrupted => None,
                            DoneStatus::Errored => error.take(),
                        });
                        done = true;
                    }
                    _ => {}
                }
                runtimes.fold(&session_id, &turn_id, &event);
                if done {
                    break;
                }
            }
            if !done && !interrupt.is_cancelled() {
                aborted = true;
                error
                    .get_or_insert_with(|| "The provider ended the turn without finishing.".into());
            }
            // Ends the provider process and its process-scoped endpoint.
            drop(stream);
        }
    }
    let live = lock(&runtimes.turns)
        .get(&session_id)
        .filter(|turn| turn.turn_id == turn_id)
        .map(|turn| (turn.assistant_id.clone(), turn.parts.clone()));
    if let Some((assistant_id, parts)) = live {
        if let Some(rpc) = runtimes.service()
            && let Err(failure) =
                rpc.authoring_record_turn(&session_id, &assistant_id, parts, aborted, error)
        {
            tracing::warn!(session_id, %failure, "could not record a Board discussion turn");
        }
        let mut turns = lock(&runtimes.turns);
        if turns
            .get(&session_id)
            .is_some_and(|turn| turn.turn_id == turn_id)
        {
            turns.remove(&session_id);
        }
    }
    runtimes.notify();
}

/// `BoardAuthoringWatch`: the current page at once, then a page after each
/// committed or live change (identical pages are skipped). Ends after the
/// frame that shows the session ended or discarded.
pub(crate) fn watch(
    store: Arc<DocsStore>,
    runtimes: Arc<AuthoringRuntimes>,
    session_id: String,
    _device_id: String,
) -> BoxStream<'static, Value> {
    struct State {
        store: Arc<DocsStore>,
        runtimes: Arc<AuthoringRuntimes>,
        session_id: String,
        commits: watch::Receiver<u64>,
        live: watch::Receiver<u64>,
        last: Option<String>,
        ended: bool,
    }
    let state = State {
        commits: store.board_changes(),
        live: runtimes.changed.subscribe(),
        store,
        runtimes,
        session_id,
        last: None,
        ended: false,
    };
    futures::stream::unfold(state, |mut state| async move {
        if state.ended {
            return None;
        }
        loop {
            if state.last.is_some() {
                tokio::select! {
                    changed = state.commits.changed() => changed.ok()?,
                    changed = state.live.changed() => changed.ok()?,
                    _ = tokio::time::sleep(WATCH_POLL) => {}
                }
                tokio::time::sleep(WATCH_COALESCE).await;
            }
            let session = state
                .store
                .board_authoring_session(&state.session_id)
                .ok()??;
            let ended = matches!(
                session.state,
                AuthoringState::Ended | AuthoringState::Discarded
            );
            let page = authoring_page(&state.store, &state.runtimes, session, None, None).ok()?;
            let value = serde_json::to_value(&page).ok()?;
            let text = value.to_string();
            if state.last.as_deref() == Some(text.as_str()) {
                continue;
            }
            state.last = Some(text);
            state.ended = ended;
            return Some((value, state));
        }
    })
    .boxed()
}
