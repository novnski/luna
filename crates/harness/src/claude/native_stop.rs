//! Stopping one native Claude child task (CC-08): the `stop_task` control
//! request (CLI 2.1.286 SDK schema: `{subtype:"stop_task", task_id}`).
//!
//! Authority: only an UNSETTLED agent spawn of the live run bound to the
//! given native session, whose native task id that run reported, can be
//! addressed; anything else is refused locally without writing to the CLI.
//!
//! Evidence: 2.1.286 answers `stop_task` for a task id that does not exist
//! with `control_response{subtype:"success"}` (init-only probe,
//! `evidence/cc08-native-agents/probes/`). An acknowledgement is therefore
//! only a *request* receipt; the child's own `task_updated`/
//! `task_notification` (status `killed`/`stopped`) settles it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::StdinMsg;
use crate::NativeTaskStop;

/// How long a stop waits for the CLI's correlated `control_response`.
const STOP_ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_REQUEST_PREFIX: &str = "luna-stop-";

static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What one run can address: live agent spawns and in-flight stop requests.
#[derive(Default)]
pub(crate) struct Stoppable {
    /// Unsettled spawn tool_use id → its native task id.
    live: HashMap<String, String>,
    /// Stop request id → the waiter for its `control_response`.
    pending: HashMap<String, oneshot::Sender<Result<(), String>>>,
}

pub(crate) type SharedStoppable = Arc<Mutex<Stoppable>>;

/// An agent spawn's native task id became known (`task_started`).
pub(crate) fn track(stoppable: &SharedStoppable, spawn: &str, task_id: &str) {
    lock(stoppable)
        .live
        .insert(spawn.to_owned(), task_id.to_owned());
}

/// The spawn settled: it can no longer be stopped.
pub(crate) fn untrack(stoppable: &SharedStoppable, spawn: &str) {
    lock(stoppable).live.remove(spawn);
}

/// Consume a `control_response` that answers one of our stop requests.
/// Returns false for every other line (the caller keeps processing it).
pub(crate) fn resolve_control_response(stoppable: &SharedStoppable, line: &str) -> bool {
    if !line.contains(STOP_REQUEST_PREFIX) || !line.contains("\"control_response\"") {
        return false;
    }
    let Ok(frame) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let response = &frame["response"];
    let (Some("control_response"), Some(request_id)) = (
        frame["type"].as_str(),
        response["request_id"]
            .as_str()
            .filter(|id| id.starts_with(STOP_REQUEST_PREFIX)),
    ) else {
        return false;
    };
    let Some(waiter) = lock(stoppable).pending.remove(request_id) else {
        return true;
    };
    let outcome = match response["subtype"].as_str() {
        Some("success") => Ok(()),
        _ => Err(response["error"]
            .as_str()
            .unwrap_or("Claude refused the stop request")
            .to_owned()),
    };
    let _ = waiter.send(outcome);
    true
}

struct LiveRun {
    registration: u64,
    stdin: mpsc::UnboundedSender<StdinMsg>,
    stoppable: SharedStoppable,
}

/// One registration of a live run (released when the run ends).
pub(crate) struct Registration {
    session_id: String,
    registration: u64,
}

/// Live runs of one `ClaudeHarness`, keyed by native session id (the
/// provider binding the engine holds for a chat).
#[derive(Clone, Default)]
pub(crate) struct LiveChildren(Arc<Mutex<HashMap<String, LiveRun>>>);

impl LiveChildren {
    /// A newer run of the same native session replaces an older one.
    pub(crate) fn register(
        &self,
        session_id: &str,
        stdin: mpsc::UnboundedSender<StdinMsg>,
        stoppable: SharedStoppable,
    ) -> Registration {
        let registration = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        lock(&self.0).insert(
            session_id.to_owned(),
            LiveRun {
                registration,
                stdin,
                stoppable,
            },
        );
        Registration {
            session_id: session_id.to_owned(),
            registration,
        }
    }

    /// Release only this run's own entry, never a newer run's.
    pub(crate) fn deregister(&self, registration: &Registration) {
        let mut runs = lock(&self.0);
        if runs
            .get(&registration.session_id)
            .is_some_and(|run| run.registration == registration.registration)
        {
            runs.remove(&registration.session_id);
        }
    }

    pub(crate) async fn stop(&self, session_id: &str, spawn: &str) -> NativeTaskStop {
        let (stdin, stoppable) = {
            let runs = lock(&self.0);
            let Some(run) = runs.get(session_id) else {
                return NativeTaskStop::Rejected(
                    "No live Claude run owns this conversation.".into(),
                );
            };
            (run.stdin.clone(), run.stoppable.clone())
        };
        let request_id = format!("{STOP_REQUEST_PREFIX}{}", uuid::Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        {
            let mut state = lock(&stoppable);
            let Some(task_id) = state.live.get(spawn).cloned() else {
                return NativeTaskStop::Rejected(
                    "This agent is not a running native child of the live Claude run.".into(),
                );
            };
            let line = json!({
                "type": "control_request",
                "request_id": request_id,
                "request": { "subtype": "stop_task", "task_id": task_id },
            })
            .to_string();
            if stdin.send(StdinMsg::line(line)).is_err() {
                return NativeTaskStop::Rejected("Claude's input closed before the stop.".into());
            }
            state.pending.insert(request_id.clone(), tx);
        }
        match tokio::time::timeout(STOP_ANSWER_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => NativeTaskStop::Requested,
            Ok(Ok(Err(error))) => NativeTaskStop::Rejected(error),
            Ok(Err(_)) => NativeTaskStop::Unknown(
                "The Claude run ended before answering the stop request.".into(),
            ),
            Err(_) => {
                lock(&stoppable).pending.remove(&request_id);
                NativeTaskStop::Unknown("Claude did not answer the stop request in time.".into())
            }
        }
    }
}
