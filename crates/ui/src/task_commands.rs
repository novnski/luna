//! Desktop client for the L07 authoritative task-command path.

use std::sync::OnceLock;

use zeron_proto::{CommandReceipt, CommandReceiptStatus, TaskCommandRead};
use zeron_rpc::{RpcError, methods};

use crate::state::EngineHandle;

fn client_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
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
    let read: TaskCommandRead = engine
        .client()
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await?;
    let session_kind = intent.get("kind").and_then(serde_json::Value::as_str);
    let control_kind = intent
        .get("command")
        .and_then(|command| command.get("kind"))
        .and_then(serde_json::Value::as_str);
    let turn_fenced = (session_kind == Some("session")
        && matches!(control_kind, Some("steer" | "interrupt")))
        || matches!(session_kind, Some("sendQueuedNow" | "steerQueuedNow"));
    let revision_fenced = matches!(
        session_kind,
        Some(
            "rename"
                | "setArchived"
                | "delete"
                | "setBranch"
                | "setCwd"
                | "setHost"
                | "setConfig"
                | "setPermission"
                | "setProviderAccount"
        )
    );
    let binding_fenced = session_kind == Some("session")
        || matches!(session_kind, Some("sendQueuedNow" | "steerQueuedNow"))
        || matches!(
            session_kind,
            Some(
                "rename"
                    | "setArchived"
                    | "delete"
                    | "setBranch"
                    | "setCwd"
                    | "setHost"
                    | "setConfig"
                    | "setPermission"
                    | "setProviderAccount"
            )
        );
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

    // One bounded retry uses the exact same serialized envelope. If the first
    // reply was lost, readback returns its durable receipt and avoids replay.
    let first = engine
        .client()
        .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, params.clone())
        .await;
    let receipt = match first {
        Ok(receipt) => receipt,
        Err(first_error) => {
            if let Some(receipt) = receipt(engine, &command_id).await? {
                receipt
            } else {
                match engine
                    .client()
                    .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, params)
                    .await
                {
                    Ok(receipt) => receipt,
                    Err(_) => receipt(engine, &command_id).await?.ok_or(first_error)?,
                }
            }
        }
    };
    Ok(receipt)
}

async fn receipt(
    engine: &EngineHandle,
    command_id: &str,
) -> Result<Option<CommandReceipt>, RpcError> {
    engine
        .client()
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            serde_json::json!({ "commandId": command_id }),
        )
        .await
}
