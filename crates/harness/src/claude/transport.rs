//! Control transport for one Claude CLI run process (CC-01 / CC-17).
//!
//! The installed CLI and the official Agent SDK speak one protocol: JSON lines
//! on the child's stdio. Conversation frames (`system`, `stream_event`,
//! `assistant`, `user`, `result`, …) are parsed by [`super::wire`] and
//! normalized by [`super::normalize`]. This module owns the transport frames
//! around them:
//!
//! - **Capabilities.** `system/init.capabilities` (and the connect-time subset
//!   on the `initialize` response) is an open set the CLI uses for feature
//!   detection. Behaviour that depends on a protocol extension is enabled from
//!   that set, never from the version string; unknown names are kept opaquely.
//! - **Outbound control requests.** Every LUNA request gets a unique id and is
//!   tracked until its `control_response` arrives. The interrupt response is
//!   decoded as a receipt (`interrupt_receipt_v1`: `still_queued`; with
//!   `cancel_queued:true`, `cancelled`). An old CLI's empty success is only an
//!   acknowledgement, and no response at all stays unconfirmed.
//! - **Inbound control requests** (one terminal reply per id, explicit errors
//!   for unsupported subtypes, `control_cancel_request` withdrawal) are owned
//!   by the CC-03 dispatcher in `controls.rs`.
//! - **Input receipts.** Every stdin user line carries a fresh v4 `uuid`.
//!   Under `msg_lifecycle_v1` the CLI reports that uuid's fate on
//!   `command_lifecycle` frames (`queued` → `started` → exactly one of
//!   `completed|cancelled|discarded|refused`). Only an exact uuid match moves
//!   a LUNA input; text equality never does.
//!
//! Shapes and semantics were taken from the 2.1.286 binary's SDK schema and
//! re-checked with initialization-only handshakes (no model turn); see
//! `crates/harness/tests/fixtures/claude/compat-2.1.286.json`.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::SteerAcceptance;

/// `interrupt` success payload carries `still_queued`.
pub(crate) const CAP_INTERRUPT_RECEIPT: &str = "interrupt_receipt_v1";
/// `interrupt` honours `cancel_queued:true` and lists `cancelled`.
pub(crate) const CAP_INTERRUPT_CANCEL_QUEUED: &str = "interrupt_cancel_queued_v1";
/// uuid-stamped user messages produce `command_lifecycle` frames.
pub(crate) const CAP_MSG_LIFECYCLE: &str = "msg_lifecycle_v1";

/// How long an explicit steer waits for its `queued` receipt before falling
/// back to the stdin-write acknowledgement (the pre-receipt behaviour). The
/// engine's own acceptance timeout is 5 s.
const ACCEPTANCE_RECEIPT_WAIT: Duration = Duration::from_secs(2);
/// Bound on tracked request ids / input uuids per run process.
const MAX_TRACKED: usize = 512;
/// Bound on echoed CLI-supplied strings (subtypes, ids) in replies and logs.
const MAX_ECHO_BYTES: usize = 128;

/// What one run process said about its own protocol surface.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ProtocolSnapshot {
    /// `system/init.claude_code_version` (informational only).
    pub cli_version: Option<String>,
    /// Every advertised capability name, including ones LUNA does not know.
    pub capabilities: BTreeSet<String>,
    /// Native permission mode the CLI reported on `system/init`.
    pub native_permission_mode: Option<String>,
}

impl ProtocolSnapshot {
    pub(crate) fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

/// Native lifecycle of one uuid-stamped user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LifecycleState {
    Queued,
    Started,
    Completed,
    Cancelled,
    Discarded,
    Refused,
}

impl LifecycleState {
    fn parse(state: &str) -> Option<Self> {
        Some(match state {
            "queued" => Self::Queued,
            "started" => Self::Started,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "discarded" => Self::Discarded,
            "refused" => Self::Refused,
            _ => return None,
        })
    }

    fn is_terminal(self) -> bool {
        self >= Self::Completed
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Discarded => "discarded",
            Self::Refused => "refused",
        }
    }
}

/// Which LUNA path wrote a stdin user line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKind {
    /// The run's opening prompt.
    Initial,
    /// Ordinary warm routing (keeps its historical local boundary).
    Ordinary,
    /// Explicit queue steering: must be confirmed, never assumed.
    Explicit,
}

/// Decoded answer to LUNA's `interrupt` control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterruptReceipt {
    /// `interrupt_receipt_v1`: uuids that survived the abort, and (when
    /// `cancel_queued` was honoured) the uuids it cancelled.
    Receipt {
        still_queued: Vec<String>,
        cancelled: Option<Vec<String>>,
    },
    /// A pre-receipt CLI's empty success: the request was handled, nothing
    /// more is known.
    Acknowledged,
    /// The CLI answered with an `error` reply.
    Refused { message: String },
}

/// Result of offering one stdout line to the transport.
pub(crate) enum Intercept {
    /// A conversation frame: hand it to `wire::parse_frame`.
    Pass,
    /// A transport frame, fully handled here except for these effects.
    Consumed(Effects),
}

/// Effects of a consumed transport frame that the run loop must apply.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    /// LUNA message ids of explicit steers the CLI drained into a turn.
    pub incorporated: Vec<String>,
    /// Every accepted (known uuid, in-order) lifecycle transition, for the
    /// CC-02 turn ledger.
    pub lifecycle: Vec<(String, LifecycleState)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outbound {
    Interrupt,
}

struct TrackedInput {
    kind: InputKind,
    luna_message_id: Option<String>,
    state: Option<LifecycleState>,
    incorporated: bool,
    acceptance: Option<(oneshot::Sender<SteerAcceptance>, Instant)>,
}

/// Per-run transport state. Created once per CLI process.
#[derive(Default)]
pub(crate) struct ControlChannel {
    next_request: u64,
    snapshot: ProtocolSnapshot,
    outbound: HashMap<String, Outbound>,
    inputs: HashMap<String, TrackedInput>,
    input_order: Vec<String>,
    interrupt: Option<InterruptReceipt>,
    interrupt_sent: bool,
    requested_permission_mode: Option<&'static str>,
}

fn bounded(text: &str) -> &str {
    if text.len() <= MAX_ECHO_BYTES {
        return text;
    }
    let mut end = MAX_ECHO_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Cheap pre-filter: only lines that may be transport frames (or `init`, whose
/// capabilities are read here) are parsed a second time. The parsed `type`
/// decides; a conversation line that merely contains these words passes on.
fn maybe_transport(line: &str) -> bool {
    [
        "\"control_response\"",
        "\"command_lifecycle\"",
        "\"keep_alive\"",
        "\"init\"",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

impl ControlChannel {
    /// `requested_permission_mode` is the native `--permission-mode` LUNA
    /// launched with, compared against what `system/init` reports.
    pub(crate) fn new(requested_permission_mode: Option<&'static str>) -> Self {
        Self {
            requested_permission_mode,
            ..Self::default()
        }
    }

    fn mint_request_id(&mut self, kind: &str) -> String {
        self.next_request += 1;
        format!("luna-{kind}-{}", self.next_request)
    }

    /// Offer one trimmed stdout line. `system/init` is read for capabilities
    /// and still passed on; transport frames are consumed.
    pub(crate) fn intercept(&mut self, line: &str) -> Intercept {
        if !maybe_transport(line) {
            return Intercept::Pass;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Intercept::Pass;
        };
        let mut effects = Effects::default();
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                if value.get("subtype").and_then(Value::as_str) == Some("init") {
                    self.observe_init(&value);
                }
                return Intercept::Pass;
            }
            "control_response" => {
                self.on_control_response(value.get("response").unwrap_or(&Value::Null))
            }
            "command_lifecycle" => {
                let uuid = value.get("command_uuid").and_then(Value::as_str);
                let state = value.get("state").and_then(Value::as_str);
                match (uuid, state.and_then(LifecycleState::parse)) {
                    (Some(uuid), Some(state)) => self.on_lifecycle(uuid, state, &mut effects),
                    _ => tracing::debug!(
                        target: "luna_harness::claude::transport",
                        state = state.map(bounded).unwrap_or(""),
                        "command_lifecycle with an unknown state or no uuid (ignored)"
                    ),
                }
            }
            "keep_alive" => {}
            _ => return Intercept::Pass,
        }
        Intercept::Consumed(effects)
    }

    fn observe_init(&mut self, init: &Value) {
        let advertised: BTreeSet<String> = init
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .take(MAX_TRACKED)
                    .map(|name| bounded(name).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let before = self.snapshot.clone();
        // A wake turn re-emits init; a capability never disappears mid-process.
        self.snapshot.capabilities.extend(advertised);
        if let Some(version) = init.get("claude_code_version").and_then(Value::as_str) {
            self.snapshot.cli_version = Some(bounded(version).to_owned());
        }
        if let Some(mode) = init.get("permissionMode").and_then(Value::as_str) {
            let mode = bounded(mode).to_owned();
            if let Some(requested) = self.requested_permission_mode
                && requested != mode
                && self.snapshot.native_permission_mode.as_deref() != Some(mode.as_str())
            {
                tracing::warn!(
                    target: "luna_harness::claude::transport",
                    requested, effective = %mode,
                    "Claude reports a different native permission mode than LUNA requested"
                );
            }
            self.snapshot.native_permission_mode = Some(mode);
        }
        if self.snapshot == before {
            return;
        }
        tracing::info!(
            target: "luna_harness::claude::transport",
            version = self.snapshot.cli_version.as_deref().unwrap_or("unknown"),
            permission_mode = self
                .snapshot
                .native_permission_mode
                .as_deref()
                .unwrap_or("unreported"),
            interrupt_receipt = self.snapshot.has(CAP_INTERRUPT_RECEIPT),
            interrupt_cancel_queued = self.snapshot.has(CAP_INTERRUPT_CANCEL_QUEUED),
            msg_lifecycle = self.snapshot.has(CAP_MSG_LIFECYCLE),
            capabilities = ?self.snapshot.capabilities,
            "claude protocol snapshot"
        );
    }

    // ---- outbound requests (LUNA → CLI) --------------------------------

    /// The interrupt request for LUNA's Stop: one round-trip that aborts the
    /// turn and cancels queued uuid-stamped input. `cancel_queued` is
    /// documented as ignored by CLIs that predate it.
    pub(crate) fn interrupt_line(&mut self) -> String {
        let request_id = self.mint_request_id("interrupt");
        self.outbound
            .insert(request_id.clone(), Outbound::Interrupt);
        self.interrupt_sent = true;
        json!({
            "type": "control_request",
            "request_id": request_id,
            "request": { "subtype": "interrupt", "cancel_queued": true },
        })
        .to_string()
    }

    fn on_control_response(&mut self, response: &Value) {
        let Some(request_id) = response.get("request_id").and_then(Value::as_str) else {
            return;
        };
        let Some(kind) = self.outbound.remove(request_id) else {
            tracing::debug!(
                target: "luna_harness::claude::transport",
                request_id = bounded(request_id),
                "control_response for a request LUNA is not waiting on (ignored)"
            );
            return;
        };
        let receipt = if response.get("subtype").and_then(Value::as_str) == Some("error") {
            InterruptReceipt::Refused {
                message: response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("control request failed")
                    .to_owned(),
            }
        } else {
            let payload = response.get("response").unwrap_or(&Value::Null);
            let uuids = |key: &str| {
                payload.get(key).and_then(Value::as_array).map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
            };
            match uuids("still_queued") {
                Some(still_queued) => InterruptReceipt::Receipt {
                    still_queued,
                    cancelled: uuids("cancelled"),
                },
                None => InterruptReceipt::Acknowledged,
            }
        };
        match kind {
            Outbound::Interrupt => {
                match &receipt {
                    InterruptReceipt::Receipt {
                        still_queued,
                        cancelled,
                    } => {
                        for uuid in cancelled.iter().flatten() {
                            self.settle_cancelled_by_stop(uuid);
                        }
                        tracing::info!(
                            target: "luna_harness::claude::transport",
                            still_queued = still_queued.len(),
                            cancelled = cancelled.as_ref().map(Vec::len),
                            "claude interrupt receipt"
                        );
                    }
                    InterruptReceipt::Acknowledged => tracing::info!(
                        target: "luna_harness::claude::transport",
                        "claude interrupt acknowledged without a receipt (older CLI)"
                    ),
                    InterruptReceipt::Refused { message } => tracing::warn!(
                        target: "luna_harness::claude::transport",
                        error = bounded(message),
                        "claude refused the interrupt request"
                    ),
                }
                self.interrupt = Some(receipt);
            }
        }
    }

    fn settle_cancelled_by_stop(&mut self, uuid: &str) {
        if let Some(input) = self.inputs.get_mut(uuid)
            && !input.state.is_some_and(LifecycleState::is_terminal)
        {
            input.state = Some(LifecycleState::Cancelled);
            if let Some((tx, _)) = input.acceptance.take() {
                let _ = tx.send(SteerAcceptance::Rejected(
                    "Claude cancelled the queued input when the run was stopped".into(),
                ));
            } else if input.kind != InputKind::Ordinary {
                tracing::info!(
                    target: "luna_harness::claude::transport",
                    uuid = bounded(uuid), kind = ?input.kind,
                    "Claude did not run an accepted input (cancelled by Stop)"
                );
            }
        }
    }

    // ---- input receipts -------------------------------------------------

    /// Stamp a fresh v4 uuid onto a `wire` user line and start tracking it.
    /// Returns the stamped line and its uuid. LUNA message ids are never sent
    /// as the native uuid (the CLI types it as a UUID).
    pub(crate) fn stamp_user_line(
        &mut self,
        line: String,
        kind: InputKind,
        luna_message_id: Option<String>,
    ) -> (String, String) {
        let uuid = uuid::Uuid::new_v4().to_string();
        let Some(rest) = line.strip_prefix('{').filter(|rest| rest.len() > 1) else {
            return (line, uuid);
        };
        let stamped = format!("{{\"uuid\":\"{uuid}\",{rest}");
        if self.inputs.len() >= MAX_TRACKED {
            let oldest = self.input_order.remove(0);
            self.inputs.remove(&oldest);
        }
        self.input_order.push(uuid.clone());
        self.inputs.insert(
            uuid.clone(),
            TrackedInput {
                kind,
                luna_message_id,
                state: None,
                incorporated: false,
                acceptance: None,
            },
        );
        (stamped, uuid)
    }

    /// An explicit steer's stdin write succeeded. With `msg_lifecycle_v1`
    /// acceptance waits for the CLI's own receipt; without it the write
    /// acknowledgement remains the evidence (pre-receipt behaviour).
    pub(crate) fn explicit_written(
        &mut self,
        uuid: &str,
        acceptance: Option<oneshot::Sender<SteerAcceptance>>,
    ) {
        let Some(tx) = acceptance else {
            return;
        };
        let lifecycle = self.snapshot.has(CAP_MSG_LIFECYCLE);
        let Some(input) = self.inputs.get_mut(uuid).filter(|_| lifecycle) else {
            let _ = tx.send(SteerAcceptance::Accepted);
            return;
        };
        match input.state {
            Some(LifecycleState::Queued | LifecycleState::Started | LifecycleState::Completed) => {
                let _ = tx.send(SteerAcceptance::Accepted);
            }
            Some(state) => {
                let _ = tx.send(SteerAcceptance::Rejected(format!(
                    "Claude reported the steer {}",
                    lifecycle_word(state)
                )));
            }
            None => input.acceptance = Some((tx, Instant::now() + ACCEPTANCE_RECEIPT_WAIT)),
        }
    }

    fn on_lifecycle(&mut self, uuid: &str, state: LifecycleState, effects: &mut Effects) {
        let Some(input) = self.inputs.get_mut(uuid) else {
            tracing::debug!(
                target: "luna_harness::claude::transport",
                uuid = bounded(uuid),
                ?state,
                "command_lifecycle for a uuid this run did not send (ignored)"
            );
            return;
        };
        if input.state.is_some_and(LifecycleState::is_terminal)
            || input
                .state
                .is_some_and(|current| current >= state && !state.is_terminal())
        {
            tracing::debug!(
                target: "luna_harness::claude::transport",
                uuid = bounded(uuid),
                ?state,
                previous = ?input.state,
                "out-of-order or duplicate command_lifecycle (ignored)"
            );
            return;
        }
        input.state = Some(state);
        effects.lifecycle.push((uuid.to_owned(), state));
        match state {
            LifecycleState::Queued | LifecycleState::Started | LifecycleState::Completed => {
                if let Some((tx, _)) = input.acceptance.take() {
                    let _ = tx.send(SteerAcceptance::Accepted);
                }
                // Only `started` proves the exact input was drained into a
                // turn; a bare terminal state can be a duplicate delivery.
                if state == LifecycleState::Started
                    && input.kind == InputKind::Explicit
                    && !input.incorporated
                    && let Some(id) = &input.luna_message_id
                {
                    input.incorporated = true;
                    effects.incorporated.push(id.clone());
                }
            }
            LifecycleState::Cancelled | LifecycleState::Discarded | LifecycleState::Refused => {
                if let Some((tx, _)) = input.acceptance.take() {
                    let _ = tx.send(SteerAcceptance::Rejected(format!(
                        "Claude reported the steer {}",
                        lifecycle_word(state)
                    )));
                } else if input.kind != InputKind::Ordinary {
                    tracing::info!(
                        target: "luna_harness::claude::transport",
                        uuid = bounded(uuid), ?state, kind = ?input.kind,
                        "Claude did not run an accepted input"
                    );
                }
            }
        }
    }

    /// Earliest pending acceptance deadline, for the run loop's timer branch.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.inputs
            .values()
            .filter_map(|input| input.acceptance.as_ref().map(|(_, at)| *at))
            .min()
    }

    /// Resolve acceptances whose receipt did not arrive in time with the
    /// stdin-write acknowledgement they would have had before receipts.
    pub(crate) fn expire_due(&mut self, now: Instant) {
        for (uuid, input) in &mut self.inputs {
            if input.acceptance.as_ref().is_some_and(|(_, at)| *at <= now)
                && let Some((tx, _)) = input.acceptance.take()
            {
                tracing::warn!(
                    target: "luna_harness::claude::transport",
                    uuid = bounded(uuid),
                    "no lifecycle receipt for an explicit steer; reporting the stdin write only"
                );
                let _ = tx.send(SteerAcceptance::Accepted);
            }
        }
    }

    /// The process is going away: unresolved acceptances become unknown, and
    /// non-terminal inputs are recorded as discarded (the documented wrapper
    /// duty on process exit).
    pub(crate) fn finish(&mut self) {
        let mut discarded = 0usize;
        for input in self.inputs.values_mut() {
            if let Some((tx, _)) = input.acceptance.take() {
                let _ = tx.send(SteerAcceptance::Unknown(
                    "Claude session ended before receipting the steer".into(),
                ));
            }
            if !input.state.is_some_and(LifecycleState::is_terminal) {
                input.state = Some(LifecycleState::Discarded);
                discarded += 1;
            }
        }
        if self.interrupt_sent && self.interrupt.is_none() {
            tracing::info!(
                target: "luna_harness::claude::transport",
                "claude interrupt was never answered (unconfirmed)"
            );
        }
        tracing::debug!(
            target: "luna_harness::claude::transport",
            inputs = self.inputs.len(),
            discarded,
            capabilities = ?self.snapshot.capabilities,
            "claude transport closed"
        );
    }
}

fn lifecycle_word(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Queued => "queued",
        LifecycleState::Started => "started",
        LifecycleState::Completed => "completed",
        LifecycleState::Cancelled => "cancelled",
        LifecycleState::Discarded => "discarded",
        LifecycleState::Refused => "refused",
    }
}

/// After an interrupted run's terminal frame, stdin is closed so the CLI can
/// flush and exit on its own; wait for that within `grace`. The caller then
/// escalates with the usual SIGTERM → SIGKILL ladder if it is still alive.
pub(crate) async fn drain_exit(child: &mut crate::process::Child, grace: Duration) {
    let drained = matches!(tokio::time::timeout(grace, child.wait()).await, Ok(Ok(_)));
    tracing::debug!(
        target: "luna_harness::claude::transport",
        drained,
        "claude stdin closed after the interrupted turn"
    );
}
