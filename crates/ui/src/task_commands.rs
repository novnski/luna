//! Desktop client for the L07 authoritative task-command path.

use std::sync::OnceLock;

use zeron_proto::{CommandReceipt, CommandReceiptStatus, TaskCommandRead};
use zeron_rpc::{RpcError, methods};

use crate::state::EngineHandle;

fn client_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

fn revision_fenced_kind(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "rename"
                | "setArchived"
                | "delete"
                | "setBranch"
                | "setCwd"
                | "setHost"
                | "setConfig"
                | "setModel"
                | "compactContext"
                | "clearContext"
                | "switchProvider"
                | "setPermission"
                | "setProviderAccount"
        )
    )
}

fn binding_fenced_kind(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "session"
                | "queueMessage"
                | "editQueuedMessage"
                | "removeQueuedMessage"
                | "sendQueuedNow"
                | "steerQueuedNow"
                | "retryRecovery"
                | "cancelRecovery"
                | "rename"
                | "setArchived"
                | "delete"
                | "setBranch"
                | "setCwd"
                | "setHost"
                | "setConfig"
                | "setModel"
                | "compactContext"
                | "clearContext"
                | "switchProvider"
                | "setPermission"
                | "setProviderAccount"
        )
    )
}

pub async fn execute(
    engine: &EngineHandle,
    task_id: &str,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    execute_with_id(engine, task_id, uuid::Uuid::new_v4().to_string(), intent).await
}

pub async fn execute_with_id(
    engine: &EngineHandle,
    task_id: &str,
    command_id: String,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    let receipt = execute_receipt_with_id(engine, task_id, command_id, intent).await?;
    match receipt.status {
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied => Ok(receipt),
        _ => Err(RpcError::Failed(
            receipt
                .reason
                .as_ref()
                .map(|reason| reason.message.clone())
                .unwrap_or_else(|| format!("task command ended as {:?}", receipt.status)),
        )),
    }
}

/// Same bounded L07 transport/recovery path, but preserve terminal rejection
/// versus pending/unknown so the composer can settle its immutable draft
/// snapshot without guessing that a lost response means the effect failed.
pub async fn execute_receipt_with_id(
    engine: &EngineHandle,
    task_id: &str,
    command_id: String,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    execute_receipt_on_with_id(engine, task_id, None, command_id, intent).await
}

pub async fn execute_receipt_on_with_id(
    engine: &EngineHandle,
    task_id: &str,
    target_device_id: Option<&str>,
    command_id: String,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    let mut snapshot_params = serde_json::json!({ "taskId": task_id });
    if let Some(target) = target_device_id {
        snapshot_params["targetDeviceId"] = target.into();
    }
    let read: TaskCommandRead = engine
        .client()
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, snapshot_params)
        .await?;
    let session_kind = intent.get("kind").and_then(serde_json::Value::as_str);
    let control_kind = intent
        .get("command")
        .and_then(|command| command.get("kind"))
        .and_then(serde_json::Value::as_str);
    let turn_fenced = (session_kind == Some("session")
        && matches!(control_kind, Some("steer" | "interrupt")))
        || matches!(session_kind, Some("sendQueuedNow" | "steerQueuedNow"));
    let revision_fenced = revision_fenced_kind(session_kind);
    let binding_fenced = binding_fenced_kind(session_kind);
    let mut params = serde_json::json!({
        "commandId": command_id,
        "actorId": engine.engine_info().device_id,
        "clientId": engine.engine_info().device_id,
        "clientSessionId": client_session_id(),
        "taskId": task_id,
        "intent": intent,
    });
    if revision_fenced {
        params["expectedTaskRevision"] = read.snapshot.task_revision.into();
    }
    if binding_fenced && let Some(generation) = read.snapshot.binding_generation {
        params["expectedBindingGeneration"] = generation.into();
    }
    if turn_fenced && let Some(turn_id) = read.snapshot.active_turn_id {
        params["expectedTurnId"] = turn_id.into();
    }
    if let Some(target) = target_device_id {
        params["targetDeviceId"] = target.into();
    }

    // One bounded retry uses the exact same serialized envelope. If the first
    // reply was lost, readback returns its durable receipt and avoids replay.
    let first = engine
        .client()
        .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, params.clone())
        .await;
    let receipt = match first {
        Ok(receipt) => receipt,
        Err(first_error) => {
            if let Some(receipt) = receipt(engine, target_device_id, &command_id).await? {
                receipt
            } else {
                match engine
                    .client()
                    .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, params)
                    .await
                {
                    Ok(receipt) => receipt,
                    Err(_) => receipt(engine, target_device_id, &command_id)
                        .await?
                        .ok_or(first_error)?,
                }
            }
        }
    };
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_intents_carry_revision_and_binding_fences() {
        for kind in [
            "setModel",
            "compactContext",
            "clearContext",
            "switchProvider",
        ] {
            assert!(revision_fenced_kind(Some(kind)), "revision fence: {kind}");
            assert!(binding_fenced_kind(Some(kind)), "binding fence: {kind}");
        }
    }
}

async fn receipt(
    engine: &EngineHandle,
    target_device_id: Option<&str>,
    command_id: &str,
) -> Result<Option<CommandReceipt>, RpcError> {
    let mut params = serde_json::json!({ "commandId": command_id });
    if let Some(target) = target_device_id {
        params["targetDeviceId"] = target.into();
    }
    engine
        .client()
        .call_as(methods::GET_TASK_COMMAND_RECEIPT, params)
        .await
}
