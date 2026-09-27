//! Local prompt stash RPC implementation. No task or provider state is created.

use std::collections::HashSet;

use luna_proto::{DraftBlobState, MAX_PROMPT_STASH_ENTRY_BYTES, PromptStashEntry};
use luna_rpc::RpcError;

use super::EngineRpc;

pub(super) fn list(rpc: &EngineRpc) -> Result<Vec<PromptStashEntry>, RpcError> {
    let mut entries = rpc
        .store
        .list_prompt_stash(&rpc.engine_info.device_id)
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    for entry in &mut entries {
        for blob in &mut entry.attachments {
            let validation = match blob.state {
                DraftBlobState::Saved => blob
                    .path
                    .as_deref()
                    .ok_or_else(|| "Saved attachment has no durable path".to_string())
                    .and_then(|path| {
                        rpc.uploads
                            .validate_draft_blob(path, blob.size, blob.sha256.as_deref())
                            .map_err(|error| error.to_string())
                    }),
                DraftBlobState::WaitingForUpload => {
                    Err("Attachment copy did not finish".to_string())
                }
                DraftBlobState::NeedsReattachment => continue,
            };
            if let Err(error) = validation {
                blob.state = DraftBlobState::NeedsReattachment;
                blob.path = None;
                blob.error = Some(error);
            }
        }
    }
    Ok(entries)
}

pub(super) fn save(rpc: &EngineRpc, entry: &PromptStashEntry) -> Result<(), RpcError> {
    if uuid::Uuid::parse_str(&entry.id).is_err()
        || entry.label.trim().is_empty()
        || entry.label.chars().count() > 120
        || (entry.text.trim().is_empty()
            && entry.attachments.is_empty()
            && entry.appshots.is_empty()
            && entry.context.review_comments.is_empty())
    {
        return Err(RpcError::BadParams("invalid saved prompt".into()));
    }
    if entry.context.quoted.is_some() || entry.context.terminal.is_some() {
        return Err(RpcError::BadParams(
            "saved prompts do not yet support quoted or terminal context".into(),
        ));
    }
    let encoded =
        serde_json::to_vec(entry).map_err(|error| RpcError::BadParams(error.to_string()))?;
    if encoded.len() > MAX_PROMPT_STASH_ENTRY_BYTES {
        return Err(RpcError::BadParams(
            "saved prompt metadata exceeds 1 MiB".into(),
        ));
    }
    let mut attachment_ids = HashSet::new();
    for blob in &entry.attachments {
        if blob.attachment_id.is_empty()
            || !attachment_ids.insert(blob.attachment_id.as_str())
            || blob.state != DraftBlobState::Saved
        {
            return Err(RpcError::BadParams(
                "saved prompt attachments must be unique and fully uploaded".into(),
            ));
        }
        let path = blob
            .path
            .as_deref()
            .ok_or_else(|| RpcError::BadParams("saved attachment has no path".into()))?;
        rpc.uploads
            .validate_draft_blob(path, blob.size, blob.sha256.as_deref())
            .map_err(|error| RpcError::BadParams(error.to_string()))?;
    }
    let mut appshot_ids = HashSet::new();
    let mut screenshot_ids = HashSet::new();
    for shot in &entry.appshots {
        if shot.id.is_empty()
            || !appshot_ids.insert(shot.id.as_str())
            || !screenshot_ids.insert(shot.screenshot_attachment_id.as_str())
            || !attachment_ids.contains(shot.screenshot_attachment_id.as_str())
        {
            return Err(RpcError::BadParams(
                "saved window capture requires one unique screenshot attachment".into(),
            ));
        }
    }
    rpc.store
        .save_prompt_stash(&rpc.engine_info.device_id, entry)
        .map_err(|error| RpcError::Failed(error.to_string()))
}

pub(super) fn delete(rpc: &EngineRpc, id: &str) -> Result<bool, RpcError> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(RpcError::BadParams("invalid saved prompt id".into()));
    }
    let paths = rpc
        .store
        .list_prompt_stash(&rpc.engine_info.device_id)
        .map_err(|error| RpcError::Failed(error.to_string()))?
        .into_iter()
        .find(|entry| entry.id == id)
        .map(|entry| {
            entry
                .saved_blob_paths()
                .map(str::to_string)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let deleted = rpc
        .store
        .delete_prompt_stash(&rpc.engine_info.device_id, id)
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    if deleted {
        for path in paths {
            let regular = std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.file_type().is_file());
            if regular && rpc.uploads.owned_blob_path(&path).is_some() {
                // The store takes an immediate writer reservation from owner
                // check through unlink. Another draft/stash can still adopt
                // this path before the check and keep it protected.
                if let Err(error) = rpc.store.remove_unowned_attachment(&path) {
                    tracing::warn!(%path, %error, "prompt stash blob cleanup deferred");
                }
            }
        }
    }
    Ok(deleted)
}
