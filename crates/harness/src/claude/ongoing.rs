//! Claude's ongoing-work observers (CC-14/15) behind one run-loop hook:
//! native goals ([`super::goals`]), structured review findings
//! ([`super::findings`]) and native schedules ([`super::schedules`]).
//!
//! They read the RAW stdout frame before the established normalizer, add
//! events, and never change or suppress what the normalizer emits. Streaming
//! deltas (the bulk of the wire) are skipped without a second parse.

use std::path::PathBuf;

use luna_proto::AgentEvent;
use serde_json::Value;

use super::findings::{FindingsTracker, ProvenanceRequest};
use super::goals::{GoalTracker, ReadbackRequest};
use super::schedules::ScheduleTracker;

pub(crate) struct Ongoing {
    enabled: bool,
    config_dir: Option<PathBuf>,
    cwd: String,
    goals: GoalTracker,
    findings: FindingsTracker,
    schedules: ScheduleTracker,
    /// The first `init` reads the session record once (resume seed).
    seeded: bool,
}

/// Follow-up work a frame needs before its events can be emitted.
pub(crate) enum Work {
    GoalSeed { session_id: String },
    GoalReadback(ReadbackRequest),
    Findings(ProvenanceRequest),
}

impl Ongoing {
    pub(crate) fn new(enabled: bool, config_dir: Option<PathBuf>, cwd: &str) -> Self {
        Self {
            enabled,
            config_dir,
            cwd: cwd.to_owned(),
            goals: GoalTracker::new(),
            findings: FindingsTracker::new(),
            schedules: ScheduleTracker::new(),
            seeded: false,
        }
    }

    /// LUNA wrote `/goal <condition>` to Claude's stdin.
    pub(crate) fn goal_requested(&mut self, condition: &str) {
        self.goals.requested(condition);
    }

    pub(crate) fn observe(&mut self, line: &str) -> (Vec<AgentEvent>, Vec<Work>) {
        if !self.enabled || line.starts_with("{\"type\":\"stream_event\"") {
            return (Vec::new(), Vec::new());
        }
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return (Vec::new(), Vec::new());
        };
        let init = frame.get("subtype").and_then(Value::as_str) == Some("init")
            && super::goals::is_root(&frame);
        if let Some(cwd) = init
            .then(|| frame.get("cwd").and_then(Value::as_str))
            .flatten()
            .filter(|cwd| !cwd.is_empty())
        {
            self.cwd = cwd.to_owned();
        }
        let seed = (init && !std::mem::replace(&mut self.seeded, true))
            .then(|| frame.get("session_id").and_then(Value::as_str))
            .flatten()
            .filter(|_| self.goals.wants_seed())
            .map(|session_id| Work::GoalSeed {
                session_id: session_id.to_owned(),
            });
        let (mut events, readback) = self.goals.observe(&frame, &self.cwd);
        let (review, provenance) = self.findings.observe(&frame, &self.cwd);
        events.extend(review);
        events.extend(self.schedules.observe(&frame));
        let mut work: Vec<Work> = seed.into_iter().collect();
        work.extend(provenance.into_iter().map(Work::Findings));
        work.extend(readback.map(Work::GoalReadback));
        (events, work)
    }

    pub(crate) async fn resolve(&mut self, work: Work) -> Vec<AgentEvent> {
        match work {
            Work::GoalSeed { session_id } => {
                let (Some(dir), cwd) = (self.config_dir.clone(), self.cwd.clone()) else {
                    return Vec::new();
                };
                let readback = tokio::task::spawn_blocking(move || {
                    super::goals::read_transcript_goal(&dir, &session_id, &cwd)
                })
                .await
                .ok()
                .flatten();
                self.goals.seed(readback)
            }
            Work::GoalReadback(request) => {
                let readback =
                    super::goals::read_after_loop(&self.goals, self.config_dir.clone(), &request)
                        .await;
                self.goals.apply_readback(readback, request.result_uuid)
            }
            Work::Findings(request) => {
                let revision = super::findings::observe_revision(&request.cwd).await;
                vec![super::findings::render(&request, &revision)]
            }
        }
    }
}
