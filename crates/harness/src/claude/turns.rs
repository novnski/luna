//! CC-02 / G3 — native turn identity for Claude Code.
//!
//! Every stdin user line LUNA writes carries a fresh native `uuid` (an
//! envelope sibling of `message`, never model-visible). Echo-capable CLIs
//! (2.1.286's schema documents it) echo the uuid(s) a turn answers on its
//! first stream/assistant frame (`user_message_uuid`) and on its result
//! (`user_message_uuid(s)`); turns Claude starts by itself (background task
//! notifications, peer messages, scheduled wakeups) echo nothing and carry a
//! non-human `origin`.
//!
//! Result ownership:
//! - OWN: the echo names a uuid LUNA sent (those sends are consumed).
//! - FOREIGN: the echo names only uuids LUNA never sent, or there is no echo
//!   and `origin.kind != "human"`.
//! - UNATTRIBUTED: neither (older runtime, or a CLI-minted turn).
//!
//! A FOREIGN result must not complete a human turn LUNA is still waiting on:
//! on an echo-capable runtime, while an initial or follow-up send is
//! unconsumed, its `Done` is withheld (usage and evidence still flow) and the
//! human turn's own first stamped frame opens a fresh segment. Without echo
//! capability the legacy rule stays (every root result completes the active
//! turn): a folded send is invisible there, and Claude turns have no quiesce
//! backstop, so withholding could strand a turn forever.

use std::collections::{BTreeMap, VecDeque};

use luna_proto::AgentEvent;
use serde_json::{Value, json};

use super::events::{
    self, CERTIFIED_ECHO_VERSION, InitInfo, NativeBody, NativeEvent, NativeFrame, ResultEnvelope,
    TurnStamp,
};
use super::normalize::Normalizer;
use super::wire::Frame;

const MAX_PENDING_SENDS: usize = 64;
const MAX_REMEMBERED_IDS: usize = 256;
const MAX_COUNTED_KINDS: usize = 128;
const RETAIN_PER_KIND: u32 = 32;
const RETAIN_UNKNOWN_PER_KIND: u32 = 8;
const RETAIN_PER_RUN: u32 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendKind {
    /// The run's first prompt (the engine opened its turn at dispatch).
    Initial,
    /// An ordinary warm follow-up (`SteerDelivery::AllowFollowUpTurn`).
    FollowUp,
    /// An explicit in-turn steer (`SteerDelivery::RequireActiveTurn`).
    Steer,
}

impl SendKind {
    fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::FollowUp => "followUp",
            Self::Steer => "steer",
        }
    }
}

#[derive(Debug, Clone)]
struct PendingSend {
    native_uuid: String,
    luna_id: Option<String>,
    kind: SendKind,
    /// A segment boundary (`Steered`) for this send is still owed to the engine.
    boundary_owed: bool,
}

/// A segment boundary the normalizer emits as `Steered`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Boundary {
    /// LUNA's message id for follow-ups (exact routed-steer correlation);
    /// the native uuid for the initial prompt (never matches a queue row).
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultOwner {
    Own,
    Foreign,
    Unattributed,
    Duplicate,
}

impl ResultOwner {
    fn label(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Foreign => "foreign",
            Self::Unattributed => "unattributed",
            Self::Duplicate => "duplicate",
        }
    }
}

#[derive(Debug)]
pub(crate) struct ResultDecision {
    pub owner: ResultOwner,
    /// Drop the root `Done` (usage/context still flow).
    pub withhold_done: bool,
    /// Drop every event for this result (duplicate / replayed history).
    pub drop_all: bool,
    pub boundaries: Vec<Boundary>,
    consumed: Vec<PendingSend>,
}

/// Per-process correlation state. Native uuids are minted per process and
/// never reused across a reconnect (`--resume` starts a fresh ledger).
#[derive(Debug, Default)]
pub(crate) struct TurnLedger {
    pending: VecDeque<PendingSend>,
    consumed: VecDeque<String>,
    seen_results: VecDeque<String>,
    echo_capable: bool,
    echo_observed: bool,
    runtime_version: Option<String>,
    withheld_foreign: u32,
}

fn remember(ring: &mut VecDeque<String>, id: String) {
    if ring.len() >= MAX_REMEMBERED_IDS {
        ring.pop_front();
    }
    ring.push_back(id);
}

impl TurnLedger {
    /// Record a written send and return its native uuid.
    pub fn register(&mut self, kind: SendKind, luna_id: Option<String>) -> String {
        let native_uuid = uuid::Uuid::new_v4().to_string();
        self.register_native(kind, luna_id, native_uuid.clone());
        native_uuid
    }

    /// Record a written send whose native uuid was minted elsewhere (one uuid
    /// per send, shared with any other per-send receipt ledger).
    pub fn register_native(
        &mut self,
        kind: SendKind,
        luna_id: Option<String>,
        native_uuid: String,
    ) {
        if self.pending.len() >= MAX_PENDING_SENDS {
            self.pending.pop_front();
        }
        self.pending.push_back(PendingSend {
            native_uuid,
            luna_id,
            kind,
            boundary_owed: kind == SendKind::FollowUp && self.echo_capable,
        });
    }

    /// Whether an ordinary follow-up's `Steered` waits for native evidence
    /// that the CLI started it (instead of the stdin write).
    pub fn defers_follow_up_boundary(&self) -> bool {
        self.echo_capable
    }

    pub fn observe_init(&mut self, init: &InitInfo) {
        if let Some(version) = &init.claude_code_version {
            self.runtime_version = Some(version.clone());
            if events::version_at_least(version, CERTIFIED_ECHO_VERSION) {
                self.echo_capable = true;
            }
        }
    }

    fn is_ours(&self, uuid: &str) -> bool {
        self.pending.iter().any(|send| send.native_uuid == uuid)
            || self.consumed.iter().any(|seen| seen == uuid)
    }

    /// A stamped root frame (or a `started` lifecycle) names the send(s) the
    /// turn answers: release any boundary still owed for them.
    pub fn on_started(&mut self, echoed: &[&str]) -> Vec<Boundary> {
        if echoed.iter().any(|uuid| self.is_ours(uuid)) {
            self.echo_observed = true;
            self.echo_capable = true;
        }
        let mut out = Vec::new();
        for uuid in echoed {
            if let Some(send) = self
                .pending
                .iter_mut()
                .find(|send| send.native_uuid == *uuid && send.boundary_owed)
            {
                send.boundary_owed = false;
                out.push(boundary_for(send));
            }
        }
        out
    }

    /// A send the CLI will never answer (`cancelled`/`discarded`/`refused`).
    /// Returns true when it was the last unconsumed send a withheld foreign
    /// result was waiting behind.
    pub fn on_abandoned(&mut self, uuid: &str) -> bool {
        let before = self.pending.len();
        self.pending.retain(|send| send.native_uuid != uuid);
        before != self.pending.len() && self.withheld_foreign > 0 && !self.awaits_human_turn()
    }

    fn awaits_human_turn(&self) -> bool {
        self.pending.iter().any(|send| send.kind != SendKind::Steer)
    }

    pub fn classify(&mut self, stamp: &TurnStamp, interrupted: bool) -> ResultDecision {
        let decision = |owner, withhold_done, drop_all| ResultDecision {
            owner,
            withhold_done,
            drop_all,
            boundaries: Vec::new(),
            consumed: Vec::new(),
        };
        if stamp.historical {
            return decision(ResultOwner::Duplicate, true, true);
        }
        if let Some(id) = &stamp.native_uuid {
            if self.seen_results.contains(id) {
                return decision(ResultOwner::Duplicate, true, true);
            }
            remember(&mut self.seen_results, id.clone());
        }
        let echoed = stamp.echoed();
        let owner = if !echoed.is_empty() {
            if echoed.iter().any(|uuid| self.is_ours(uuid)) {
                self.echo_observed = true;
                self.echo_capable = true;
                ResultOwner::Own
            } else {
                ResultOwner::Foreign
            }
        } else if stamp
            .origin
            .as_ref()
            .is_some_and(|origin| !origin.is_human())
        {
            ResultOwner::Foreign
        } else {
            ResultOwner::Unattributed
        };
        let mut out = decision(owner, false, false);
        match owner {
            ResultOwner::Own => {
                let (consumed, kept): (Vec<_>, Vec<_>) = self
                    .pending
                    .drain(..)
                    .partition(|send| echoed.contains(&send.native_uuid.as_str()));
                self.pending = kept.into();
                out.consumed = consumed;
            }
            ResultOwner::Foreign => {
                if !interrupted && self.echo_capable && self.awaits_human_turn() {
                    out.withhold_done = true;
                    self.withheld_foreign += 1;
                    // The engine's open segment now holds foreign output: the
                    // human turn's first stamped frame must split it.
                    for send in &mut self.pending {
                        if send.kind == SendKind::Initial {
                            send.boundary_owed = true;
                        }
                    }
                }
            }
            ResultOwner::Unattributed => {
                // Legacy FIFO: the oldest human send is taken as answered.
                if let Some(position) = self
                    .pending
                    .iter()
                    .position(|send| send.kind != SendKind::Steer)
                    && let Some(send) = self.pending.remove(position)
                {
                    out.consumed.push(send);
                }
            }
            ResultOwner::Duplicate => {}
        }
        if !out.withhold_done {
            self.withheld_foreign = 0;
        }
        for send in &mut out.consumed {
            if send.boundary_owed {
                send.boundary_owed = false;
                out.boundaries.push(boundary_for(send));
            }
            remember(&mut self.consumed, send.native_uuid.clone());
        }
        out
    }

    fn summary(&self) -> Value {
        json!({
            "echoCapable": self.echo_capable,
            "echoObserved": self.echo_observed,
            "runtimeVersion": self.runtime_version,
            "pendingSends": self.pending.iter().map(|send| json!({
                "nativeUuid": send.native_uuid,
                "lunaId": send.luna_id,
                "kind": send.kind.label(),
                "boundaryOwed": send.boundary_owed,
            })).collect::<Vec<_>>(),
            "withheldForeign": self.withheld_foreign,
        })
    }
}

fn boundary_for(send: &PendingSend) -> Boundary {
    Boundary {
        correlation_id: match send.kind {
            SendKind::Initial => Some(send.native_uuid.clone()),
            _ => send.luna_id.clone(),
        },
    }
}

/// Frame counts and retention budgets (counted always, retained up to caps).
#[derive(Debug, Default)]
struct EvidenceBudget {
    counts: BTreeMap<String, u64>,
    retained: BTreeMap<String, u32>,
    retained_total: u32,
}

impl EvidenceBudget {
    fn count(&mut self, kind: &str) {
        let key = if self.counts.contains_key(kind) || self.counts.len() < MAX_COUNTED_KINDS {
            kind.to_owned()
        } else {
            "overflow".to_owned()
        };
        *self.counts.entry(key).or_default() += 1;
    }

    fn admit(&mut self, kind: &str, per_kind: u32) -> bool {
        self.count(kind);
        if self.retained_total >= RETAIN_PER_RUN {
            return false;
        }
        let retained = self.retained.entry(kind.to_owned()).or_default();
        if *retained >= per_kind {
            return false;
        }
        *retained += 1;
        self.retained_total += 1;
        true
    }
}

/// CC-02 state carried by the per-run [`Normalizer`].
#[derive(Debug, Default)]
pub(crate) struct NativeTracker {
    pub ledger: TurnLedger,
    budget: EvidenceBudget,
    /// A root assistant/rate-limit error was already surfaced this turn.
    turn_error_surfaced: bool,
    /// A withheld foreign `Done`, released if the human send it waited
    /// behind is abandoned by the CLI.
    withheld_done: Option<AgentEvent>,
    init_retained: bool,
}

impl NativeTracker {
    /// Retained evidence for a frame the established normalizer does not
    /// project. Returns boundaries owed (from `command_lifecycle started`)
    /// and the evidence/released events.
    fn native_frame(&mut self, frame: NativeFrame) -> (Vec<Boundary>, Vec<AgentEvent>) {
        let NativeFrame {
            kind,
            native_uuid,
            body,
        } = frame;
        let mut boundaries = Vec::new();
        let mut out = Vec::new();
        match body {
            NativeBody::Progress => self.budget.count(&kind),
            NativeBody::Init(init) => {
                self.ledger.observe_init(&init);
                self.budget.count(&kind);
                if !self.init_retained {
                    self.init_retained = true;
                    let mut payload = events::to_payload(&init);
                    if let Value::Object(map) = &mut payload {
                        map.insert("ledger".into(), self.ledger.summary());
                    }
                    out.push(events::evidence(kind, native_uuid, payload));
                }
            }
            NativeBody::Event(event) => {
                if let NativeEvent::CommandLifecycle(lifecycle) = &event {
                    match lifecycle.state.as_str() {
                        "started" => {
                            boundaries = self.ledger.on_started(&[lifecycle.command_uuid.as_str()]);
                        }
                        "cancelled" | "discarded" | "refused" => {
                            out.extend(self.abandoned(&lifecycle.command_uuid));
                        }
                        _ => {}
                    }
                }
                if self.budget.admit(&kind, RETAIN_PER_KIND) {
                    out.insert(
                        0,
                        events::evidence(kind, native_uuid, events::to_payload(&event)),
                    );
                }
            }
            NativeBody::Unprojected(sample) => {
                let kind = format!("native:{kind}");
                if self.budget.admit(&kind, RETAIN_UNKNOWN_PER_KIND) {
                    out.push(events::evidence(
                        kind,
                        native_uuid,
                        events::to_payload(&sample),
                    ));
                }
            }
            NativeBody::Unknown(sample) => {
                let kind = format!("unknown:{kind}");
                tracing::debug!(target: "luna_harness::claude", %kind, bytes = sample.bytes, "unknown Claude frame retained as evidence");
                if self.budget.admit(&kind, RETAIN_UNKNOWN_PER_KIND) {
                    out.push(events::evidence(
                        kind,
                        native_uuid,
                        events::to_payload(&sample),
                    ));
                }
            }
        }
        (boundaries, out)
    }

    /// A send the CLI will never run: release a foreign `Done` that was only
    /// withheld because this send was still pending.
    fn abandoned(&mut self, native_uuid: &str) -> Option<AgentEvent> {
        if self.ledger.on_abandoned(native_uuid) {
            self.withheld_done.take()
        } else {
            None
        }
    }

    /// Evidence for a stdout line that failed to parse (non-JSON or a known
    /// frame type with an invalid shape).
    pub fn invalid_line(&mut self, line: &str, error: &serde_json::Error) -> Vec<AgentEvent> {
        let value = serde_json::from_str::<Value>(line).ok();
        let kind = match value
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
        {
            Some(kind) => format!("invalid:{}", events::bounded(kind, 64)),
            None => "invalid:non_json".to_owned(),
        };
        if !self.budget.admit(&kind, RETAIN_UNKNOWN_PER_KIND) {
            return Vec::new();
        }
        let mut payload = events::to_payload(&events::raw_sample(line, value.as_ref()));
        if let Value::Object(map) = &mut payload {
            map.insert(
                "error".into(),
                Value::String(events::bounded(&error.to_string(), 512)),
            );
        }
        vec![events::evidence(kind, None, payload)]
    }

    /// Apply result ownership to the established normalizer's result events.
    fn settle_result(
        &mut self,
        envelope: &ResultEnvelope,
        core: Vec<AgentEvent>,
        interrupted: bool,
    ) -> (Vec<Boundary>, Vec<AgentEvent>) {
        self.budget.count("result");
        let decision = self.ledger.classify(&envelope.stamp, interrupted);
        let mut payload = events::to_payload(envelope);
        if let Value::Object(map) = &mut payload {
            map.insert("owner".into(), Value::String(decision.owner.label().into()));
            map.insert("withheld".into(), Value::Bool(decision.withhold_done));
            map.insert(
                "consumed".into(),
                Value::Array(
                    decision
                        .consumed
                        .iter()
                        .map(|send| {
                            json!({
                                "nativeUuid": send.native_uuid,
                                "lunaId": send.luna_id,
                                "kind": send.kind.label(),
                            })
                        })
                        .collect(),
                ),
            );
            map.insert("ledger".into(), self.ledger.summary());
            map.insert("frameCounts".into(), json!(self.budget.counts));
        }
        let record = events::evidence("result", envelope.stamp.native_uuid.clone(), payload);
        if decision.drop_all {
            return (Vec::new(), vec![record]);
        }
        let mut out = vec![record];
        let error_surfaced = std::mem::take(&mut self.turn_error_surfaced);
        for event in core {
            match event {
                AgentEvent::Done { .. } if decision.withhold_done => {
                    self.withheld_done = Some(event);
                }
                AgentEvent::Done {
                    status,
                    result,
                    error,
                    session_id,
                } => {
                    self.withheld_done = None;
                    // `success` + `is_error`: the turn ended on an API error
                    // and `result` holds its text. Keep the turn's status (an
                    // Errored Done would start recovery/retry) but surface the
                    // error once and never present it as the answer.
                    let api_error = envelope.subtype == "success" && envelope.is_error;
                    if api_error && !error_surfaced {
                        let message = if envelope.api_error_status == Some(529) {
                            "Claude API is overloaded (529). Try again shortly.".to_owned()
                        } else {
                            result
                                .as_deref()
                                .filter(|text| !text.trim().is_empty())
                                .map(|text| events::bounded(text, 2048))
                                .unwrap_or_else(|| "Claude ended the turn on an API error.".into())
                        };
                        out.push(AgentEvent::Error { message });
                    }
                    out.push(AgentEvent::Done {
                        status,
                        result: if api_error { None } else { result },
                        error,
                        session_id,
                    });
                }
                other => out.push(other),
            }
        }
        (decision.boundaries, out)
    }
}

impl Normalizer {
    fn boundary_event(&mut self, boundary: Boundary) -> AgentEvent {
        let (previous, next) = self.rotate_for_steer();
        AgentEvent::Steered {
            user_message_id: boundary.correlation_id,
            assistant_message_id: Some(previous),
            next_assistant_message_id: Some(next),
        }
    }

    /// Lifecycle receipt hooks for a transport layer that consumes
    /// `command_lifecycle` frames before they reach `normalize_native`:
    /// `started` releases a deferred follow-up boundary; a terminal
    /// non-run state releases a foreign `Done` withheld behind that send.
    #[allow(dead_code)] // Called by a lifecycle-intercepting transport (cc01) at integration.
    pub fn native_input_lifecycle(&mut self, native_uuid: &str, state: &str) -> Vec<AgentEvent> {
        match state {
            "started" => self
                .native
                .ledger
                .on_started(&[native_uuid])
                .into_iter()
                .map(|boundary| self.boundary_event(boundary))
                .collect(),
            "cancelled" | "discarded" | "refused" => {
                self.native.abandoned(native_uuid).into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    /// Production entry point: the established [`Normalizer::normalize`]
    /// plus CC-02 turn identity, decoded lifecycle evidence and bounded
    /// retention of everything it does not project.
    pub fn normalize_native(&mut self, mut frame: Frame, interrupted: bool) -> Vec<AgentEvent> {
        let mut boundaries = Vec::new();
        let mut evidence = Vec::new();
        let mut envelope = None;
        match &mut frame {
            Frame::StreamEvent(f) if f.parent_tool_use_id.is_none() => {
                boundaries = self.native.ledger.on_started(&f.stamp.echoed());
            }
            Frame::Assistant(f) if f.parent_tool_use_id.is_none() => {
                boundaries = self.native.ledger.on_started(&f.stamp.echoed());
            }
            Frame::System(f) => {
                if let Some(native) = f.native.take() {
                    let (owed, retained) = self.native.native_frame(*native);
                    boundaries = owed;
                    evidence = retained;
                }
            }
            Frame::Native(native) => {
                let native = std::mem::replace(
                    native,
                    NativeFrame {
                        kind: String::new(),
                        native_uuid: None,
                        body: NativeBody::Progress,
                    },
                );
                let (owed, retained) = self.native.native_frame(native);
                boundaries = owed;
                evidence = retained;
            }
            Frame::Result(f) => envelope = Some(*std::mem::take(&mut f.envelope)),
            Frame::Other => self.native.budget.count("transport"),
            _ => {}
        }
        let mut out: Vec<AgentEvent> = Vec::new();
        let core = self.normalize(frame, interrupted);
        let (late_boundaries, core) = match &envelope {
            Some(envelope) => self.native.settle_result(envelope, core, interrupted),
            None => {
                if core
                    .iter()
                    .any(|event| matches!(event, AgentEvent::Error { .. }))
                {
                    self.native.turn_error_surfaced = true;
                }
                (Vec::new(), core)
            }
        };
        for boundary in boundaries.into_iter().chain(late_boundaries) {
            out.push(self.boundary_event(boundary));
        }
        // Result evidence precedes its Done so the journal still ends on Done.
        out.extend(evidence);
        out.extend(core);
        out
    }
}

/// Add the native `uuid` envelope field to a stdin user line. The uuid is a
/// sibling of `message`, so it never reaches the model.
pub(crate) fn stamp_line(line: &str, native_uuid: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(mut map)) => {
            map.insert("uuid".into(), Value::String(native_uuid.to_owned()));
            Value::Object(map).to_string()
        }
        _ => line.to_owned(),
    }
}
