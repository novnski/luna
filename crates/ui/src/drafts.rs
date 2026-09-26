//! L08 composer-draft RPC helpers. Drafts always use the directly attached
//! engine, so they stay on the composing device/profile.

use luna_proto::{
    ComposerDraft, ComposerDraftRead, ComposerDraftSaveOutcome, DraftBlob, DraftBlobState,
    PromptStashEntry,
};
use luna_rpc::{RpcError, methods};
use sha2::{Digest, Sha256};

use crate::state::EngineHandle;

pub const SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

pub fn waiting_blob(attachment_id: &str, name: &str) -> DraftBlob {
    DraftBlob {
        attachment_id: attachment_id.into(),
        name: name.into(),
        path: None,
        sha256: None,
        size: None,
        state: DraftBlobState::WaitingForUpload,
        error: None,
    }
}

pub fn saved_blob(attachment_id: &str, name: &str, path: String, bytes: &[u8]) -> DraftBlob {
    DraftBlob {
        attachment_id: attachment_id.into(),
        name: name.into(),
        path: Some(path),
        sha256: Some(format!("{:x}", Sha256::digest(bytes))),
        size: Some(bytes.len() as u64),
        state: DraftBlobState::Saved,
        error: None,
    }
}

pub fn failed_blob(attachment_id: &str, name: &str, error: impl Into<String>) -> DraftBlob {
    DraftBlob {
        attachment_id: attachment_id.into(),
        name: name.into(),
        path: None,
        sha256: None,
        size: None,
        state: DraftBlobState::NeedsReattachment,
        error: Some(error.into()),
    }
}

pub async fn read(
    engine: &EngineHandle,
    task_id: Option<&str>,
    draft_id: Option<&str>,
) -> Result<ComposerDraftRead, RpcError> {
    engine
        .client()
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({ "taskId": task_id, "draftId": draft_id }),
        )
        .await
}

pub async fn save(
    engine: &EngineHandle,
    draft: &ComposerDraft,
) -> Result<ComposerDraftSaveOutcome, RpcError> {
    engine
        .client()
        .call_as(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({ "draft": draft }),
        )
        .await
}

pub async fn list_stash(engine: &EngineHandle) -> Result<Vec<PromptStashEntry>, RpcError> {
    engine
        .client()
        .call_as(methods::LIST_PROMPT_STASH, serde_json::json!({}))
        .await
}

pub async fn save_stash(engine: &EngineHandle, entry: &PromptStashEntry) -> Result<(), RpcError> {
    engine
        .client()
        .call(
            methods::SAVE_PROMPT_STASH,
            serde_json::json!({ "entry": entry }),
        )
        .await?;
    Ok(())
}

pub async fn delete_stash(engine: &EngineHandle, id: &str) -> Result<bool, RpcError> {
    let reply = engine
        .client()
        .call(
            methods::DELETE_PROMPT_STASH,
            serde_json::json!({ "id": id }),
        )
        .await?;
    reply
        .get("deleted")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| RpcError::Failed("prompt stash delete receipt is missing".into()))
}

pub async fn begin_submission(
    engine: &EngineHandle,
    command_id: &str,
    task_id: Option<&str>,
    snapshot: &ComposerDraft,
) -> Result<(), RpcError> {
    engine
        .client()
        .call(
            methods::BEGIN_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId": command_id,
                "taskId": task_id,
                "snapshot": snapshot,
            }),
        )
        .await?;
    Ok(())
}

pub async fn settle_submission(
    engine: &EngineHandle,
    command_id: &str,
    outcome: &str,
    queue_id: Option<&str>,
) -> Result<(), RpcError> {
    engine
        .client()
        .call(
            methods::SETTLE_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId": command_id,
                "outcome": outcome,
                "queueId": queue_id,
            }),
        )
        .await?;
    Ok(())
}

pub async fn discard_recoverable_submission(
    engine: &EngineHandle,
    command_id: &str,
) -> Result<(bool, Option<u64>), RpcError> {
    let reply = engine
        .client()
        .call(
            methods::SETTLE_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId": command_id,
                "outcome": "discarded",
            }),
        )
        .await?;
    let discarded = reply
        .get("discarded")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| RpcError::Failed("draft discard receipt is missing".into()))?;
    let cleared_revision = reply
        .get("clearedRevision")
        .and_then(serde_json::Value::as_u64);
    Ok((discarded, cleared_revision))
}
