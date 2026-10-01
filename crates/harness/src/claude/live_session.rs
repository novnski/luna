//! CC-06 per-run state for the Claude run loop: delivery receipts, result
//! ownership, live accounting, launch readback and staged live switches.
//! `run_session` calls into this at four points (start, each stdout line, each
//! steer, the switch deadline) and keeps its own control flow.

use std::collections::HashSet;
use std::time::Duration;

use luna_proto::{AgentEvent, ProviderAccountingReport, ReasoningLevel, RuntimeReadbackSource};
use serde_json::Value;
use tokio::sync::mpsc;

use super::delivery::{self, DeliveryLedger, LineKind, ResultEcho, ResultOwner, SideFrame};
use super::normalize::Normalizer;
use super::runtime_controls::{self as controls, ControlIds, ControlReply, PlannedSwitch};
use super::{Cc06Session, StdinMsg, accounting, apply_ultrathink, wire};
use crate::{SteerDelivery, SteerMessage};

/// Native acknowledgment budget for one live switch (acks + readback).
const SWITCH_TIMEOUT: Duration = Duration::from_secs(10);

struct PendingSwitch {
    steer: SteerMessage,
    plan: PlannedSwitch,
    awaiting: HashSet<String>,
    readback: Option<String>,
    deadline: tokio::time::Instant,
}

pub(super) struct LiveSession {
    title_only: bool,
    ledger: DeliveryLedger,
    ids: ControlIds,
    staged: controls::SharedStaged,
    launch: Option<(String, Option<String>, Option<String>)>,
    pending: Option<PendingSwitch>,
    session_id: Option<String>,
}

/// What the loop does with one stdout line.
pub(super) enum LineOutcome {
    /// A side frame was consumed; publish these events and read on.
    Consumed(Vec<AgentEvent>),
    /// A live switch failed: the prompt was not written. Stop steering and let
    /// the CLI exit so the engine re-dispatches the message with its new config.
    SwitchFailed(Vec<AgentEvent>),
    /// Normalize the frame as usual. `prefix` goes out first; `foreign` means a
    /// result that must not complete the outstanding human turn.
    Normalize {
        prefix: Vec<AgentEvent>,
        foreign: bool,
    },
}

/// What the loop does with one steer from the mailbox.
pub(super) enum SteerRoute {
    /// Parked behind a live switch; nothing to write yet.
    Pending,
    /// Write it now with this native uuid.
    Write(Box<SteerMessage>, String),
}

fn accounting_event(report: ProviderAccountingReport) -> AgentEvent {
    AgentEvent::ProviderAccounting {
        report: Box::new(report),
    }
}

impl LiveSession {
    pub fn start(
        cc06: Cc06Session,
        title_only: bool,
        stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
    ) -> Self {
        let mut session = Self {
            title_only,
            ledger: DeliveryLedger::default(),
            ids: ControlIds::default(),
            staged: cc06.staged,
            launch: None,
            pending: None,
            session_id: None,
        };
        session
            .ledger
            .sent(cc06.first_uuid, None, LineKind::Initial);
        if let Some((model, effort)) = cc06.launch_readback {
            let id = session.ids.mint("launch-readback");
            let _ = stdin_tx.send(StdinMsg::line(controls::request_line(
                &id,
                controls::get_settings_request(),
            )));
            session.launch = Some((id, model, effort));
        }
        session
    }

    pub fn switch_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn switch_deadline(&self) -> Option<tokio::time::Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    pub fn interrupted(&mut self) {
        self.ledger.clear();
    }

    pub fn record_sent(&mut self, uuid: String, engine_id: Option<String>, explicit: bool) {
        let kind = if explicit {
            LineKind::Explicit
        } else {
            LineKind::Routed
        };
        self.ledger.sent(uuid, engine_id, kind);
    }

    pub fn record_unsent(&mut self, uuid: &str) {
        self.ledger.unsent(uuid);
    }

    /// Inspect one non-empty stdout line before normalization.
    #[allow(clippy::too_many_arguments)]
    pub fn on_line(
        &mut self,
        line: &str,
        norm: &mut Normalizer,
        stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
        reasoning: &mut Option<ReasoningLevel>,
        explicit_steer_unconfirmed: &mut bool,
        interrupted: bool,
    ) -> LineOutcome {
        if let Some(side) = delivery::side_frame(line) {
            return match side {
                SideFrame::Replay { uuid } => {
                    let mut out = Vec::new();
                    if let Some(recorded) = self.ledger.on_replay(&uuid)
                        && recorded.kind == LineKind::Explicit
                        && let Some(engine_id) = recorded.engine_id
                    {
                        // Native recording of THIS input: the exact receipt the
                        // engine settles a durable steer row with.
                        *explicit_steer_unconfirmed = false;
                        let (prev, next) = norm.rotate_for_steer();
                        out.push(AgentEvent::Steered {
                            user_message_id: Some(engine_id),
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        });
                    }
                    LineOutcome::Consumed(out)
                }
                SideFrame::ControlResponse(frame) => match ControlReply::parse(&frame) {
                    Some(reply) => self.on_reply(reply, norm, stdin_tx, reasoning),
                    None => LineOutcome::Consumed(Vec::new()),
                },
            };
        }
        let interesting = line.contains("\"result\"")
            || line.contains("\"rate_limit_event\"")
            || line.contains("\"api_retry\"");
        if self.title_only || !interesting {
            return LineOutcome::Normalize {
                prefix: Vec::new(),
                foreign: false,
            };
        }
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return LineOutcome::Normalize {
                prefix: Vec::new(),
                foreign: false,
            };
        };
        if let Some(id) = frame.get("session_id").and_then(Value::as_str)
            && !id.is_empty()
        {
            self.session_id = Some(id.to_owned());
        }
        let mut foreign = false;
        if frame.get("type").and_then(Value::as_str) == Some("result") {
            if interrupted {
                self.ledger.clear();
            } else {
                let echo: ResultEcho = serde_json::from_value(frame.clone()).unwrap_or_default();
                let outstanding = self.ledger.has_outstanding();
                foreign = self.ledger.classify(&echo) == ResultOwner::Foreign && outstanding;
                if foreign {
                    tracing::debug!(
                        target: "luna_harness::claude",
                        origin = ?echo.origin_kind(),
                        "foreign result while a human turn is outstanding; Done withheld"
                    );
                }
            }
        }
        LineOutcome::Normalize {
            prefix: accounting::report_for(&frame)
                .map(accounting_event)
                .into_iter()
                .collect(),
            foreign,
        }
    }

    fn on_reply(
        &mut self,
        reply: ControlReply,
        norm: &mut Normalizer,
        stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
        reasoning: &mut Option<ReasoningLevel>,
    ) -> LineOutcome {
        if self
            .launch
            .as_ref()
            .is_some_and(|(id, _, _)| *id == reply.request_id)
            && let Some((_, model, effort)) = self.launch.take()
        {
            let readback = controls::readback(
                RuntimeReadbackSource::Launch,
                self.session_id.clone(),
                model,
                effort,
                Some(&reply),
            );
            return LineOutcome::Consumed(vec![accounting_event(
                ProviderAccountingReport::Runtime(readback),
            )]);
        }
        let Some(pending) = self.pending.as_mut() else {
            return LineOutcome::Consumed(Vec::new());
        };
        if pending.awaiting.remove(&reply.request_id) {
            if !reply.ok {
                let detail = reply.error.unwrap_or_else(|| "rejected".into());
                return self.fail(stdin_tx, format!("live switch rejected: {detail}"));
            }
            if pending.awaiting.is_empty() {
                let id = self.ids.mint("switch-readback");
                let _ = stdin_tx.send(StdinMsg::line(controls::request_line(
                    &id,
                    controls::get_settings_request(),
                )));
                if let Some(pending) = self.pending.as_mut() {
                    pending.readback = Some(id);
                }
            }
            return LineOutcome::Consumed(Vec::new());
        }
        if pending.readback.as_deref() != Some(reply.request_id.as_str()) {
            return LineOutcome::Consumed(Vec::new());
        }
        let Some(pending) = self.pending.take() else {
            return LineOutcome::Consumed(Vec::new());
        };
        let readback = controls::readback(
            RuntimeReadbackSource::LiveSwitch,
            self.session_id.clone(),
            pending.plan.expect_model.clone(),
            pending.plan.expect_effort.clone(),
            Some(&reply),
        );
        if !readback.matches {
            let detail = readback.detail.clone().unwrap_or_default();
            self.close_for_restart(stdin_tx, &detail);
            return LineOutcome::SwitchFailed(vec![accounting_event(
                ProviderAccountingReport::Runtime(readback),
            )]);
        }
        // Applied and read back: only now does the prompt reach the CLI.
        *reasoning = pending.plan.reasoning;
        let mut out = vec![accounting_event(ProviderAccountingReport::Runtime(
            readback,
        ))];
        out.extend(self.write_routed(pending.steer, norm, stdin_tx, *reasoning));
        LineOutcome::Consumed(out)
    }

    /// Ordinary warm write (historical immediate boundary semantics) with a uuid.
    fn write_routed(
        &mut self,
        steer: SteerMessage,
        norm: &mut Normalizer,
        stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
        reasoning: Option<ReasoningLevel>,
    ) -> Vec<AgentEvent> {
        let uuid = delivery::native_uuid_for(steer.message_id.as_deref());
        let line = delivery::with_uuid(
            wire::user_message_line(&apply_ultrathink(reasoning, &steer.prompt)),
            &uuid,
        );
        if stdin_tx.send(StdinMsg::line(line)).is_err() {
            return Vec::new();
        }
        self.record_sent(uuid, steer.message_id, false);
        let (prev, next) = norm.rotate_for_steer();
        vec![AgentEvent::Steered {
            user_message_id: None,
            assistant_message_id: Some(prev),
            next_assistant_message_id: Some(next),
        }]
    }

    fn fail(&mut self, stdin_tx: &mpsc::UnboundedSender<StdinMsg>, detail: String) -> LineOutcome {
        let plan = self.pending.as_ref().map(|pending| pending.plan.clone());
        let readback = luna_proto::RuntimeReadback {
            provider: luna_proto::UsageProvider::Claude,
            native_session_id: self.session_id.clone(),
            observed_at_ms: accounting::now_ms(),
            source: RuntimeReadbackSource::LiveSwitch,
            requested_model: plan.as_ref().and_then(|plan| plan.expect_model.clone()),
            effective_model: None,
            requested_effort: plan.as_ref().and_then(|plan| plan.expect_effort.clone()),
            effective_effort: None,
            matches: false,
            detail: Some(detail.clone()),
        };
        self.close_for_restart(stdin_tx, &detail);
        LineOutcome::SwitchFailed(vec![accounting_event(ProviderAccountingReport::Runtime(
            readback,
        ))])
    }

    /// Fail closed: never write the prompt under the old configuration. Closing
    /// stdin lets the idle CLI exit; the engine re-dispatches the unconfirmed
    /// routed message as a fresh run with the new configuration.
    fn close_for_restart(&mut self, stdin_tx: &mpsc::UnboundedSender<StdinMsg>, detail: &str) {
        tracing::warn!(target: "luna_harness::claude", %detail, "live model/effort switch failed; restarting");
        self.pending = None;
        let _ = stdin_tx.send(StdinMsg::Close);
    }

    /// The switch deadline elapsed without acks/readback.
    pub fn switch_timed_out(&mut self, stdin_tx: &mpsc::UnboundedSender<StdinMsg>) -> LineOutcome {
        self.fail(
            stdin_tx,
            "live switch was not acknowledged in time".to_owned(),
        )
    }

    /// Route one steer: a staged live switch parks it until the CLI applied and
    /// read back the new model/effort; everything else is written at once.
    pub fn route_steer(
        &mut self,
        steer: SteerMessage,
        stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
        reasoning: &mut Option<ReasoningLevel>,
    ) -> SteerRoute {
        let plan = (steer.delivery == SteerDelivery::AllowFollowUpTurn)
            .then(|| controls::take_staged(&self.staged, steer.message_id.as_deref()))
            .flatten();
        let Some(plan) = plan else {
            let uuid = delivery::native_uuid_for(steer.message_id.as_deref());
            return SteerRoute::Write(Box::new(steer), uuid);
        };
        if !plan.needs_native_change() {
            *reasoning = plan.reasoning;
            let uuid = delivery::native_uuid_for(steer.message_id.as_deref());
            return SteerRoute::Write(Box::new(steer), uuid);
        }
        let mut awaiting = HashSet::new();
        if let Some(model) = &plan.set_model {
            let id = self.ids.mint("set-model");
            let _ = stdin_tx.send(StdinMsg::line(controls::request_line(
                &id,
                controls::set_model_request(model),
            )));
            awaiting.insert(id);
        }
        if !plan.settings.is_empty() {
            let id = self.ids.mint("apply-settings");
            let _ = stdin_tx.send(StdinMsg::line(controls::request_line(
                &id,
                controls::apply_settings_request(&plan.settings),
            )));
            awaiting.insert(id);
        }
        self.pending = Some(PendingSwitch {
            steer,
            plan,
            awaiting,
            readback: None,
            deadline: tokio::time::Instant::now() + SWITCH_TIMEOUT,
        });
        SteerRoute::Pending
    }
}
