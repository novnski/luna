//! `ListCommands` execution scope (G1, E23).
//!
//! Claude Code loads different settings, custom commands and output styles per
//! workspace, policy and account (Ask/Auto run `--restricted`). A command catalog
//! is only truthful when it is discovered under the scope the next run in that
//! chat or space would launch with. This module resolves that scope with the
//! run path's own sources: the chat's cwd, the provider-capability and machine
//! clamp of `permissions::resolve_preview_for`, the child-thread ceiling, and the
//! provider account's authentication instance. Unscoped requests (older clients)
//! keep the legacy behaviour: no workspace, the default mode.

use std::path::PathBuf;

use luna_harness::CommandScope;
use luna_proto::{HarnessId, PermissionMode, WorkspaceTarget};
use luna_rpc::RpcError;
use serde::Deserialize;

use crate::{AgentAccounts, WorkspaceFiles, WorkspaceHost};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListCommandsParams {
    pub harness: HarnessId,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub space_id: Option<String>,
    /// Checkout inside `space_id` (a worktree), as for workspace file RPCs.
    #[serde(default)]
    pub path: Option<String>,
    /// The composer's selected mode; defaults to the chat's configured mode.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
}

pub(crate) async fn resolve(
    params: &ListCommandsParams,
    workspace: &WorkspaceHost,
    files: &WorkspaceFiles,
    device_id: &str,
    accounts: &AgentAccounts,
) -> Result<CommandScope, RpcError> {
    let failed = |error: crate::EngineError| RpcError::Failed(error.to_string());
    let chat = params
        .chat_id
        .as_deref()
        .map(|id| workspace.chat(id))
        .transpose()
        .map_err(failed)?
        .flatten();
    if params.chat_id.is_some() && chat.is_none() {
        return Err(RpcError::BadParams("unknown command discovery task".into()));
    }
    let cwd: Option<PathBuf> = match (&chat, &params.space_id) {
        (Some(chat), _) => {
            if chat.device_id != device_id {
                return Err(RpcError::BadParams(
                    "command discovery task belongs to another device".into(),
                ));
            }
            // The run launches in the chat's own cwd (worktree or projectless folder).
            chat.cwd.as_deref().map(PathBuf::from)
        }
        (None, Some(space_id)) => Some(
            files
                .resolve_target(&WorkspaceTarget {
                    chat_id: None,
                    space_id: Some(space_id.clone()),
                    checkout_path: params.path.clone(),
                })
                .await
                .map_err(RpcError::from)?
                .root,
        ),
        (None, None) => None,
    };
    let config = chat.as_ref().and_then(|chat| chat.config.as_ref());
    let requested = params
        .permission_mode
        .or(config.map(|config| config.permission_mode))
        .unwrap_or_default();
    let model = config.and_then(|config| config.model.as_deref());
    let mut mode =
        crate::permissions::resolve_preview_for(params.harness, model, requested).effective_mode;
    if let Some(child) = chat.as_ref().and_then(|chat| chat.child_thread.as_ref()) {
        let parent = workspace
            .chat(&child.parent_task_id)
            .ok()
            .flatten()
            .and_then(|parent| parent.config)
            .map(|config| config.effective_permission_mode)
            .unwrap_or(child.permission_ceiling);
        mode = mode.min(child.permission_ceiling).min(parent);
    }
    let account = config.and_then(|config| config.provider_account_id.as_deref());
    let auth_instance = accounts
        .resolve_execution(params.harness, account)
        .ok()
        .map(|account| account.auth_instance_id);
    Ok(CommandScope {
        cwd,
        permission_mode: mode,
        auth_instance,
    })
}
