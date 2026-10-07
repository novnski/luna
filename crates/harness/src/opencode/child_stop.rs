//! Stopping one native OpenCode child (OC-11, Claude CC-08 parity): the
//! engine's per-child Stop reaches [`Harness::stop_native_child`], which asks
//! the live run bound to that root session to interrupt the child session
//! (`POST /api/session/{child}/interrupt`).
//!
//! Authority: only an UNSETTLED spawn of the live run bound to the given
//! native session can be addressed. The run loop owns the child tree, so it
//! resolves the spawn id to its child session; anything else is refused
//! locally without an HTTP request.
//!
//! Evidence: `{interrupted: true}` only says the server stopped an active
//! execution. It is a request receipt, like Claude's `stop_task` ack: the
//! chip settles on the child's own `session.execution.interrupted`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::oneshot;

use super::*;
use crate::NativeTaskStop;

/// How long a stop waits for the run loop and the server's answer.
const STOP_ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One per-child stop, answered by the run loop.
pub(super) struct ChildStopRequest {
    /// The spawn chip id (`ToolCall::id` of the `Agent: …` call).
    pub(super) spawn: String,
    pub(super) reply: oneshot::Sender<NativeTaskStop>,
}

struct LiveRun {
    registration: u64,
    stops: mpsc::UnboundedSender<ChildStopRequest>,
}

/// Live runs of one `OpencodeHarness`, keyed by root session id (the
/// provider binding the engine holds for a chat).
#[derive(Clone, Default)]
pub(super) struct LiveRuns(Arc<Mutex<HashMap<String, LiveRun>>>);

/// A run's registration; dropping it releases only its own entry.
pub(super) struct LiveRegistration {
    runs: LiveRuns,
    session_id: String,
    registration: u64,
}

impl Drop for LiveRegistration {
    fn drop(&mut self) {
        let mut runs = lock(&self.runs.0);
        if runs
            .get(&self.session_id)
            .is_some_and(|run| run.registration == self.registration)
        {
            runs.remove(&self.session_id);
        }
    }
}

impl LiveRuns {
    /// A newer run of the same session replaces an older one.
    pub(super) fn register(
        &self,
        session_id: &str,
        stops: mpsc::UnboundedSender<ChildStopRequest>,
    ) -> LiveRegistration {
        let registration = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        lock(&self.0).insert(
            session_id.to_owned(),
            LiveRun {
                registration,
                stops,
            },
        );
        LiveRegistration {
            runs: self.clone(),
            session_id: session_id.to_owned(),
            registration,
        }
    }

    pub(super) async fn stop(&self, session_id: &str, spawn: &str) -> NativeTaskStop {
        let stops = {
            let runs = lock(&self.0);
            let Some(run) = runs.get(session_id) else {
                return NativeTaskStop::Rejected(
                    "No live OpenCode run owns this conversation.".into(),
                );
            };
            run.stops.clone()
        };
        let (reply, answer) = oneshot::channel();
        if stops
            .send(ChildStopRequest {
                spawn: spawn.to_owned(),
                reply,
            })
            .is_err()
        {
            return NativeTaskStop::Rejected("The OpenCode run ended before the stop.".into());
        }
        match tokio::time::timeout(STOP_ANSWER_TIMEOUT, answer).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => NativeTaskStop::Unknown(
                "The OpenCode run ended before answering the stop request.".into(),
            ),
            Err(_) => {
                NativeTaskStop::Unknown("OpenCode did not answer the stop request in time.".into())
            }
        }
    }
}

/// Interrupt one child session on a detached server handle and translate the
/// answer. `Requested` is a receipt only; the child's terminal settles it.
pub(super) async fn interrupt_child(
    server: Server,
    child_session: String,
    dir: Option<String>,
) -> NativeTaskStop {
    match tokio::time::timeout(
        STOP_ANSWER_TIMEOUT,
        server.interrupt_session(&child_session, dir.as_deref()),
    )
    .await
    {
        Ok(Ok(true)) => NativeTaskStop::Requested,
        Ok(Ok(false)) => {
            NativeTaskStop::Rejected("OpenCode reports nothing running in this agent.".into())
        }
        Ok(Err(error)) => {
            NativeTaskStop::Unknown(format!("OpenCode did not confirm the stop: {error}"))
        }
        Err(_) => {
            NativeTaskStop::Unknown("OpenCode did not answer the stop request in time.".into())
        }
    }
}

/// Answer one stop request from the run loop: refuse anything that is not a
/// live child of this run, otherwise interrupt it in the background.
pub(super) fn answer_stop(
    request: ChildStopRequest,
    children: &ChildTree,
    server: &Server,
    dir: Option<&str>,
) {
    let Some(child_session) = children.stop_target(&request.spawn) else {
        let _ = request.reply.send(NativeTaskStop::Rejected(
            "This agent is not a running native child of the live OpenCode run.".into(),
        ));
        return;
    };
    let server = server.detached();
    let dir = dir.map(str::to_owned);
    tokio::spawn(async move {
        let outcome = interrupt_child(server, child_session, dir).await;
        let _ = request.reply.send(outcome);
    });
}

/// Root Stop: every live BACKGROUND child is stopped through the same
/// per-child interrupt (foreground children stop natively with the root).
pub(super) fn stop_background_children(children: &ChildTree, server: &Server, dir: Option<&str>) {
    for child_session in children.stop_background() {
        let server = server.detached();
        let dir = dir.map(str::to_owned);
        tokio::spawn(async move {
            let outcome = interrupt_child(server, child_session.clone(), dir).await;
            tracing::debug!(
                target: "luna_harness::opencode",
                "root stop: background child {child_session}: {outcome:?}"
            );
        });
    }
}
