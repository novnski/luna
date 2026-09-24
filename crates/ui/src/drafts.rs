//! L08 composer-draft RPC helpers. Drafts always use the directly attached
//! engine, so they stay on the composing device/profile.

use luna_proto::{
    ComposerDraft, ComposerDraftRead, ComposerDraftSaveOutcome, DraftBlob, DraftBlobState,
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
