//! Desktop client for the L07 authoritative task-command path.

use std::sync::OnceLock;

use luna_proto::{CommandReasonCode, CommandReceipt, CommandReceiptStatus, TaskCommandRead};
use luna_rpc::{RpcError, methods};

use crate::state::EngineHandle;

fn client_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

fn revision_fenced_kind(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "nativeReview"
                | "rename"
                | "setPinned"
                | "setRead"
                | "setSettled"
                | "markSeen"
                | "moveProject"
                | "setArchived"
                | "delete"
                | "setBranch"
                | "setCwd"
                | "setHost"
                | "setConfig"
                | "setModel"
                | "goal"
                | "compactContext"
                | "clearContext"
                | "switchProvider"
                | "setPermission"
                | "setProviderAccount"
                | "forkTask"
                | "editEarlierMessage"
                | "restoreHistoryRecovery"
                | "rewindConversation"
                | "reconcileConversationRewind"
                | "applyContextTransfer"
        )
    )
}

fn binding_fenced_kind(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "session"
                | "nativeReview"
                | "queueMessage"
                | "editQueuedMessage"
                | "removeQueuedMessage"
                | "sendQueuedNow"
                | "steerQueuedNow"
                | "retryRecovery"
                | "cancelRecovery"
                | "rename"
                | "setPinned"
                | "setRead"
                | "setSettled"
                | "markSeen"
                | "moveProject"
                | "setArchived"
                | "delete"
                | "setBranch"
                | "setCwd"
                | "setHost"
                | "setConfig"
                | "setModel"
                | "goal"
                | "compactContext"
                | "clearContext"
                | "switchProvider"
                | "setPermission"
                | "setProviderAccount"
                | "forkTask"
                | "editEarlierMessage"
                | "restoreHistoryRecovery"
                | "rewindConversation"
                | "reconcileConversationRewind"
                | "applyContextTransfer"
        )
    )
}

/// These intents set a desired, reversible value. Re-reading the task before
/// one retry is safe when an unrelated update (often markSeen) wins the race.
/// Commands that depend on a reviewed history/turn or positional pin anchors
/// must instead ask the person to review the changed state.
fn retryable_revision_conflict(intent: &serde_json::Value) -> bool {
    match intent.get("kind").and_then(serde_json::Value::as_str) {
        Some("rename" | "setRead" | "setSettled" | "markSeen" | "setArchived" | "moveProject") => {
            true
        }
        Some("setPinned") => {
            intent.get("afterTaskId").is_none() && intent.get("beforeTaskId").is_none()
        }
        _ => false,
    }
}

pub(crate) fn failure_message(receipt: &CommandReceipt, fallback: &str) -> String {
    match receipt.reason.as_ref().map(|reason| reason.code) {
        Some(CommandReasonCode::StaleTaskRevision) =>
            "This task changed while the action was being applied. Review it and try again.".into(),
        Some(CommandReasonCode::StaleBindingGeneration) =>
            "This task's provider session changed. Reopen the task and try again.".into(),
        Some(CommandReasonCode::StaleTurn) =>
            "The active run changed. Review the task before trying again.".into(),
        Some(CommandReasonCode::ActiveTurnConflict) =>
            "This task is still running. Wait for it to finish, then try again.".into(),
        Some(CommandReasonCode::QueueRevisionConflict | CommandReasonCode::QueueClaimed | CommandReasonCode::QueueMissing) =>
            "The queued message changed. Review the queue before trying again.".into(),
        Some(CommandReasonCode::ProviderAcceptanceUnknown) =>
            "LUNA could not confirm whether the provider received this action. Check the task before retrying.".into(),
        _ => receipt
            .recovery_action
            .clone()
            .or_else(|| receipt.reason.as_ref().map(|reason| reason.message.clone()))
            .unwrap_or_else(|| fallback.to_string()),
    }
}

/// Apply exactly a host-reviewed context transfer on its destination
/// (`preview.target`), fenced by the destination revision and binding
/// generation the host reviewed. `command_id` is kept by the caller so a lost
/// answer is recovered by reading its receipt, never by sending again.
pub async fn apply_context_transfer(
    engine: &EngineHandle,
    preview: &luna_proto::ContextTransferPreview,
    command_id: String,
) -> Result<CommandReceipt, RpcError> {
    let target = &preview.target;
    let snapshot = luna_proto::TaskCommandSnapshot {
        task_id: target.task_id.clone(),
        task_revision: target.task_revision,
        task_sequence: 0,
        binding_id: target.binding_id.clone(),
        binding_generation: target.binding_generation,
        active_turn_id: None,
    };
    let intent = serde_json::json!({
        "kind": "applyContextTransfer",
        "request": preview.request,
        "previewId": preview.preview_id,
    });
    execute_receipt_on_with_snapshot(
        engine,
        &target.task_id,
        None,
        command_id,
        intent,
        Some(snapshot),
    )
    .await
}

/// A command's durable receipt, if the host has one (Check status).
pub async fn read_receipt(
    engine: &EngineHandle,
    command_id: &str,
) -> Result<Option<CommandReceipt>, RpcError> {
    receipt(engine, None, command_id).await
}

pub async fn execute(
    engine: &EngineHandle,
    task_id: &str,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    execute_with_id(engine, task_id, uuid::Uuid::new_v4().to_string(), intent).await
}

pub async fn execute_reviewed(
    engine: &EngineHandle,
    task_id: &str,
    snapshot: luna_proto::TaskCommandSnapshot,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    execute_reviewed_on(engine, task_id, None, Some(snapshot), intent).await
}

/// Addressed to the task's execution host, so that host (not a possibly newer
/// local engine that only queues the command) parses and admits the intent.
/// Without a reviewed snapshot, the host's current snapshot supplies fences.
pub async fn execute_reviewed_on(
    engine: &EngineHandle,
    task_id: &str,
    target_device_id: Option<&str>,
    snapshot: Option<luna_proto::TaskCommandSnapshot>,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    let receipt = execute_receipt_on_with_snapshot(
        engine,
        task_id,
        target_device_id,
        uuid::Uuid::new_v4().to_string(),
        intent,
        snapshot,
    )
    .await?;
    match receipt.status {
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied => Ok(receipt),
        _ => Err(RpcError::Failed(failure_message(
            &receipt,
            "Could not update this task.",
        ))),
    }
}

pub async fn execute_with_id(
    engine: &EngineHandle,
    task_id: &str,
    command_id: String,
    intent: serde_json::Value,
) -> Result<CommandReceipt, RpcError> {
    let mut receipt = execute_receipt_with_id(engine, task_id, command_id, intent.clone()).await?;
    if receipt.status == CommandReceiptStatus::Conflict
        && receipt.reason.as_ref().map(|reason| reason.code)
            == Some(CommandReasonCode::StaleTaskRevision)
        && retryable_revision_conflict(&intent)
    {
        tracing::info!(
            task_id,
            "task changed during reversible metadata action; retrying once"
        );
        receipt =
            execute_receipt_with_id(engine, task_id, uuid::Uuid::new_v4().to_string(), intent)
                .await?;
    }
    match receipt.status {
        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied => Ok(receipt),
        _ => Err(RpcError::Failed(failure_message(
            &receipt,
            "Could not update this task.",
        ))),
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
    execute_receipt_on_with_snapshot(engine, task_id, target_device_id, command_id, intent, None)
        .await
}

async fn execute_receipt_on_with_snapshot(
    engine: &EngineHandle,
    task_id: &str,
    target_device_id: Option<&str>,
    command_id: String,
    intent: serde_json::Value,
    reviewed_snapshot: Option<luna_proto::TaskCommandSnapshot>,
) -> Result<CommandReceipt, RpcError> {
    let mut snapshot_params = serde_json::json!({ "taskId": task_id });
    if let Some(target) = target_device_id {
        snapshot_params["targetDeviceId"] = target.into();
    }
    let snapshot = match reviewed_snapshot {
        Some(snapshot) => snapshot,
        None => {
            engine
                .client()
                .call_as::<TaskCommandRead>(methods::GET_TASK_COMMAND_SNAPSHOT, snapshot_params)
                .await?
                .snapshot
        }
    };
    let session_kind = intent.get("kind").and_then(serde_json::Value::as_str);
    let control_kind = intent
        .get("command")
        .and_then(|command| command.get("kind"))
        .and_then(serde_json::Value::as_str);
    let turn_fenced = (session_kind == Some("session")
        && matches!(control_kind, Some("steer" | "interrupt")))
        || matches!(session_kind, Some("sendQueuedNow" | "steerQueuedNow"));
    let revision_fenced = revision_fenced_kind(session_kind)
        || (session_kind == Some("session")
            && intent
                .pointer("/command/request/review")
                .is_some_and(|review| !review.is_null()));
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
        params["expectedTaskRevision"] = snapshot.task_revision.into();
    }
    if binding_fenced && let Some(generation) = snapshot.binding_generation {
        params["expectedBindingGeneration"] = generation.into();
    }
    if turn_fenced && let Some(turn_id) = snapshot.active_turn_id {
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

    #[test]
    fn organization_intents_carry_revision_and_binding_fences() {
        for kind in [
            "rename",
            "setPinned",
            "setRead",
            "markSeen",
            "setArchived",
            "moveProject",
            "forkTask",
            "editEarlierMessage",
            "restoreHistoryRecovery",
            "rewindConversation",
            "reconcileConversationRewind",
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
