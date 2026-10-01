//! Controls share the running app-server connection. A cold metadata client
//! may persist/read a goal, but cannot start the loaded runtime's continuation.
//!
//! "Send as goal" therefore always runs on a LOADED thread: Codex's
//! `thread/goal/set {status: active}` calls `start_turn_if_idle` itself
//! (probes p1/p3 in `evidence/goal-parity/probes`), and the goal turn carries
//! the objective as hidden goal context, never as a user item.
use crate::{
    HarnessError,
    jsonrpc::{RpcClient, RpcRequestFailure},
};
use luna_proto::{GoalBudget, GoalOperation, GoalStartRequest, ThreadGoal};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

type ClientRegistry = Mutex<HashMap<(PathBuf, String), (u64, RpcClient)>>;
static CLIENTS: OnceLock<ClientRegistry> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
fn clients() -> &'static ClientRegistry {
    CLIENTS.get_or_init(Mutex::default)
}

pub(super) struct Registration {
    key: (PathBuf, String),
    generation: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut clients = clients().lock().unwrap_or_else(|p| p.into_inner());
        if clients
            .get(&self.key)
            .is_some_and(|(generation, _)| *generation == self.generation)
        {
            clients.remove(&self.key);
        }
    }
}
pub(super) fn register(home: &Path, thread: &str, client: &RpcClient) -> Registration {
    let key = (home.to_owned(), thread.to_owned());
    let generation = NEXT.fetch_add(1, Ordering::Relaxed);
    clients()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone(), (generation, client.clone()));
    Registration { key, generation }
}
pub(super) fn live_client(home: &Path, thread: &str) -> Option<RpcClient> {
    clients()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(home.to_owned(), thread.to_owned()))
        .map(|(_, client)| client.clone())
}

pub(super) fn request(
    thread: &str,
    operation: &GoalOperation,
) -> Result<(&'static str, Value), HarnessError> {
    operation
        .validate()
        .map_err(|error| HarnessError::Protocol(error.into()))?;
    let mut params = json!({"threadId": thread});
    let method = match operation {
        GoalOperation::Get => "thread/goal/get",
        GoalOperation::Clear => "thread/goal/clear",
        GoalOperation::Set {
            objective,
            status,
            token_budget,
        } => {
            if let Some(objective) = objective {
                params["objective"] = objective.clone().into();
            }
            if let Some(status) = status {
                params["status"] = serde_json::to_value(status).unwrap();
            }
            if let Some(budget) = token_budget {
                params["tokenBudget"] = match budget {
                    GoalBudget::Unlimited => Value::Null,
                    GoalBudget::Limited { tokens } => (*tokens).into(),
                };
            }
            "thread/goal/set"
        }
    };
    Ok((method, params))
}

pub(super) fn decode(thread: &str, reply: &Value) -> Result<Option<ThreadGoal>, HarnessError> {
    let value = reply
        .get("goal")
        .ok_or_else(|| HarnessError::Protocol("Codex goal reply omitted goal".into()))?;
    if value.is_null() {
        return Ok(None);
    }
    let goal: ThreadGoal = serde_json::from_value(value.clone())
        .map_err(|error| HarnessError::Protocol(format!("Invalid Codex goal reply: {error}")))?;
    if goal.thread_id != thread {
        return Err(HarnessError::Protocol(
            "Codex goal reply targeted another thread".into(),
        ));
    }
    Ok(Some(goal))
}

pub(super) async fn execute(
    client: &RpcClient,
    thread: &str,
    operation: &GoalOperation,
) -> Result<Option<ThreadGoal>, HarnessError> {
    let (method, params) = request(thread, operation)?;
    let reply = client
        .request_classified(method, params)
        .await
        .map_err(|error| match error {
            crate::jsonrpc::RpcRequestFailure::Rejected(message) => {
                HarnessError::RejectedBeforeAcceptance(message)
            }
            crate::jsonrpc::RpcRequestFailure::Unknown(message) => HarnessError::Protocol(message),
        })?;
    match operation {
        GoalOperation::Clear => {
            // cleared=false means the goal was already absent. Always read back.
            if !reply.get("cleared").is_some_and(Value::is_boolean) {
                return Err(HarnessError::Protocol(
                    "Codex goal clear reply omitted cleared".into(),
                ));
            }
            decode(
                thread,
                &client
                    .request("thread/goal/get", json!({"threadId": thread}))
                    .await?,
            )
        }
        _ => decode(thread, &reply),
    }
}

/// Codex's own replace-and-start sequence (TUI `ReplaceExisting`, Python SDK
/// `start_goal_operation`): clear any previous goal, then set the new one
/// active. A loaded idle thread starts the goal turn by itself; a running turn
/// becomes goal-active and Codex continues after it. A failure after dispatch
/// keeps its Unknown classification and is never retried.
pub(super) async fn start(
    client: &RpcClient,
    thread: &str,
    goal: &GoalStartRequest,
) -> Result<Option<ThreadGoal>, RpcRequestFailure> {
    goal.validate()
        .map_err(|error| RpcRequestFailure::Rejected(error.into()))?;
    let cleared = client
        .request_classified("thread/goal/clear", json!({"threadId": thread}))
        .await?;
    if !cleared.get("cleared").is_some_and(Value::is_boolean) {
        return Err(RpcRequestFailure::Unknown(
            "Codex goal clear reply omitted cleared".into(),
        ));
    }
    let mut params = json!({
        "threadId": thread,
        "objective": goal.objective,
        "status": "active",
    });
    if let Some(budget) = &goal.token_budget {
        params["tokenBudget"] = match budget {
            GoalBudget::Unlimited => Value::Null,
            GoalBudget::Limited { tokens } => (*tokens).into(),
        };
    }
    let reply = client.request_classified("thread/goal/set", params).await?;
    decode(thread, &reply).map_err(|error| RpcRequestFailure::Unknown(error.to_string()))
}

/// Before `thread/resume` the thread is not loaded in this app-server, so a
/// clear cannot interrupt anything. Clearing first prevents resume from
/// auto-starting the PREVIOUS goal's continuation (probe p4).
pub(super) async fn clear_before_load(client: &RpcClient, thread: &str) -> Result<(), String> {
    client
        .request_classified("thread/goal/clear", json!({"threadId": thread}))
        .await
        .map(|_| ())
        .map_err(|failure| failure.message().to_owned())
}
