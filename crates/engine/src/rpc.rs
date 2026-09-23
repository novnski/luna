//! EngineRpc — the engine-side `RpcService`: sessions + docs + the workspace-doc
//! entity surface.
//!
//! Methods (feature-inventory §2):
//! - `ListHarnesses` → `[HarnessDescriptor]`
//! - `ListModels {harness}` → `[Model]`
//! - `QueueCommand {chatId, command}` → `{commandId}` (durable doc command)
//! - `WatchDocMessages {chatId}` → stream of joined `SessionMessageEntry[]`,
//!   re-emitted on every doc change
//! - `WatchChats` / `WatchDevices` → streams of the workspace doc's entity rows
//! - `WatchSessions` → stream of `Session[]`: this engine's live statuses merged with
//!   remote devices' workspace session rows
//! - `Mutate {op, …}` → `{ok}` — workspace entity mutations (createChat, renameChat,
//!   setChatArchived, deleteChat, renameDevice, markChatSeen)
//! - `EngineInfo` → `{deviceId, workspaceScope}` — this runtime's fixed identity
//!   and data boundary (never forwarded)
//! - `LocalDevice` → `{deviceId}` — legacy engine identity (never forwarded)
//! - AuthRpc (feature-inventory §2): `AuthStatus` (stream), `SignIn`/`SignInHeadless` →
//!   `{url}`, `CompleteSignIn {code}`, `SignOut`, `ListOrgs`, `CreateOrg {name}`,
//!   `SelectOrg {organizationId}`
//! - Repos (§3.5): `ListRepos`, `AddRepo {path}`, `CloneRepo {url}`,
//!   `CreateRepo {name}`, `ListBranches {repoPath}` (default branch first),
//!   `ListFolders {path?}`, `CreateWorktree {repoPath, branch}`, `DeleteWorktree
//!   {repoPath, worktreePath}`; `WatchCheckoutDiffs` → stream of `CheckoutDiff[]`
//! - Workspace files: lazy directory listing, recursive path search, bounded text
//!   reads, hash-guarded writes, and a checkout-scoped filesystem change stream.
//! - Terminals (§3.4): `OpenTerminal {chatId, cols, rows}` → `TerminalSession`,
//!   `SubscribeTerminal {terminalId, afterSeq?}` → stream of `TerminalEvent`
//!   (replay then live tail), `WriteTerminal {terminalId, data}`, `ResizeTerminal`,
//!   `CloseTerminal`. M5 is single-user local: per-user owner checks land with
//!   real multi-account auth in M6.
//! - Agent accounts (§3.7): `ListAgentAccounts {forceUsage?}` →
//!   `AgentAccountsSnapshot`, `ActivateAgentAccount`/`ForgetAgentAccount`
//!   `{harness, accountId}` → snapshot, `StartAgentLogin {harness}` →
//!   `{loginId, url, mode}`, `CompleteAgentLogin {loginId, code}` → snapshot,
//!   `PollAgentLogin {loginId}`, `CancelAgentLogin {loginId}`.
//! - Uploads (§3.7): `UploadChunk {uploadId, data, seq?}`,
//!   `UploadCommit {uploadId, fileName}` → `{path}`,
//!   `ReadAttachmentChunk {path, offset}` → `{name, mimeType, data, nextOffset,
//!   done}` (path-jailed to the uploads dir + workspace-known chat cwds).
//!
//! ## Device-addressed routing (`targetDeviceId`, feature-inventory §2.1)
//!
//! ControlRpc methods are relay-forwardable: params may carry `targetDeviceId`. When it
//! names another device, the call is forwarded verbatim over that device's relay DO via
//! the [`LinkCache`] — the remote engine sees its own id and handles locally, so the
//! forward can never loop. Streaming methods are proxied by re-subscribing remotely and
//! piping items. To make another method device-addressable, nothing per-method is needed
//! beyond listing it in [`forwardable`] (and [`is_stream_method`] if it streams);
//! handlers stay transport-agnostic. This includes the workspace file surface,
//! whose checkout always lives on the routed target device.

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, RegistryDoc, SessionCommandEntry,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry,
};
use zeron_proto::{
    AttentionMutationRequest, AttentionMutationResult, AttentionProducerAvailability,
    AttentionReadRequest, AttentionSnapshot, AttentionTransitionBatch, AttentionTransitionClaim,
    BackupArchiveRequest, BackupAvailability, BackupConfigureRequest, BackupKeyConfigureRequest,
    BackupStatus, BindingOrigin, Chat, ChatConfig, CommandReasonCode, CommandReceipt,
    CommandReceiptReason, CommandReceiptStatus, CompactOperation, ComposerDraft, ComposerDraftRead,
    ContextBoundary, ContextBoundaryKind, ContextControlState, ContextHandoff,
    ContextOperationStatus, ConversationForkBoundaryKind, DraftBlobState, EngineInfo,
    FollowUpBehavior, HarnessId, HistoryCapabilityView, HistoryCursor, HistoryProjectPolicy,
    HistoryRebuildRequest, HistoryRebuildResult, HistoryRecoveryView, HistorySearchRequest,
    ImportOwnership, PermissionMode, ProviderForkCapability, ProviderForkRequest, TaskCommandEvent,
    TaskCommandSnapshot, TaskLineage, ToolCall, UsageRequest, WorkspaceScope,
};
use zeron_rpc::{LinkCache, RpcError, RpcReply, RpcService, methods, parse_params};
use zeron_sync::DocsStore;

use crate::agent_accounts::AgentAccounts;
use crate::auth::Auth;
use crate::backup_control::{BackupControl, BackupControlError};
use crate::change_requests::CheckoutChangeRequests;
use crate::cross_task_tools::{
    CrossTaskAuthority, CrossTaskBackend, CrossTaskCapabilities, CrossTaskControlGrant,
    CrossTaskError,
};
use crate::diff_sync::CheckoutDiffSync;
use crate::doc_host::DocHost;
use crate::history_operations::{
    capability_for, load_recovery, plan_history, prepare_recovery, unavailable_reason,
};
use crate::registry::HarnessRegistry;
use crate::repos::{Repos, home_dir};
use crate::sessions::SessionsEngine;
use crate::terminals::Terminals;
use crate::uploads::Uploads;
use crate::workspace_host::{TaskProjectionError, WorkspaceHost};

const FILE_SEARCH_RPC_TIMEOUT: Duration = Duration::from_secs(6);
const FILE_SEARCH_FEATURED_PATHS: usize = 32;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatParams {
    chat_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdentityParams {
    task_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSettingsParams {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    target_device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveInstructionSettingsParams {
    #[serde(default)]
    project_id: Option<String>,
    text: String,
    expected_revision: u64,
    #[serde(default)]
    target_device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntegrationInventoryParams {
    provider: HarnessId,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    target_device_id: Option<String>,
}

fn require_local_integration_target(target: Option<&str>, local: &str) -> Result<(), RpcError> {
    if target.is_some_and(|id| id != local) {
        return Err(RpcError::Failed(
            "LUNA integration settings and inventory are local to the owning device".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewProviderTaskImportParams {
    provider_id: HarnessId,
    provider_task_id: String,
    ownership: ImportOwnership,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdoptProviderTaskParams {
    preview_id: String,
    #[serde(default)]
    config: Option<ChatConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListModelsParams {
    harness: HarnessId,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetHarnessEnabledParams {
    harness: HarnessId,
    enabled: bool,
}

async fn update_harness_enabled<F>(
    registry: &HarnessRegistry,
    harness: HarnessId,
    enabled: bool,
    sign_out: F,
) -> Result<(), RpcError>
where
    F: std::future::Future<Output = Result<(), zeron_harness::HarnessError>>,
{
    let enabled_harnesses = registry.enabled_set();
    let was_enabled = enabled_harnesses.contains(&harness);
    if was_enabled && !enabled && harness == HarnessId::Antigravity {
        if enabled_harnesses.len() == 1 {
            return Err(RpcError::Failed(
                "cannot disable the last enabled harness".into(),
            ));
        }
        sign_out.await.map_err(|error| {
            RpcError::Failed(format!(
                "Antigravity sign-out failed; it remains enabled so you can retry: {error}"
            ))
        })?;
    }
    registry
        .set_enabled(harness, enabled)
        .map_err(RpcError::Failed)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueCommandParams {
    chat_id: String,
    command: SessionCommandPayload,
    /// Queued attachments (bytes already committed locally as `pending://`
    /// refs) the engine delivers to a remote host AFTER the command is
    /// durably queued — never as a gate in front of it.
    #[serde(default)]
    transfers: Vec<crate::uploads::AttachmentTransfer>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskCommandParams {
    command_id: String,
    #[serde(default = "default_actor")]
    actor_id: String,
    #[serde(default = "default_client")]
    client_id: String,
    #[serde(default = "default_client_session")]
    client_session_id: String,
    task_id: String,
    #[serde(default)]
    expected_task_revision: Option<u64>,
    #[serde(default)]
    expected_binding_generation: Option<u32>,
    #[serde(default)]
    expected_turn_id: Option<String>,
    #[serde(default = "crate::now_ms")]
    issued_at_ms: i64,
    intent: TaskCommandIntent,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryReviewToken {
    task_revision: u64,
    binding_generation: u32,
    provider_session_id: String,
    #[serde(default)]
    provider_account_id: Option<String>,
    cwd: String,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    message_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum TaskCommandIntent {
    #[serde(rename_all = "camelCase")]
    CreateChat {
        #[serde(default)]
        space_id: Option<String>,
        #[serde(default)]
        device_id: Option<String>,
        #[serde(default)]
        config: Option<ChatConfig>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        lineage: TaskLineage,
    },
    #[serde(rename_all = "camelCase")]
    Session {
        command: SessionCommandPayload,
        #[serde(default)]
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    },
    Rename {
        title: String,
    },
    #[serde(rename_all = "camelCase")]
    SetPinned {
        pinned: bool,
        #[serde(default)]
        after_task_id: Option<String>,
        #[serde(default)]
        before_task_id: Option<String>,
    },
    SetRead {
        read: bool,
    },
    SetArchived {
        archived: bool,
    },
    #[serde(rename_all = "camelCase")]
    MoveProject {
        destination_space_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ForkTask {
        new_task_id: String,
        boundary: ConversationForkBoundaryKind,
        #[serde(default)]
        message_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        initial_prompt: Option<String>,
        #[serde(default)]
        config: Option<ChatConfig>,
        #[serde(default)]
        source_link: Option<String>,
        review: HistoryReviewToken,
    },
    #[serde(rename_all = "camelCase")]
    EditEarlierMessage {
        message_id: String,
        text: String,
        review: HistoryReviewToken,
    },
    #[serde(rename_all = "camelCase")]
    RestoreHistoryRecovery {
        recovery_id: String,
        review: HistoryReviewToken,
    },
    Delete {},
    #[serde(rename_all = "camelCase")]
    SetBranch {
        branch: String,
    },
    #[serde(rename_all = "camelCase")]
    SetCwd {
        cwd: String,
    },
    #[serde(rename_all = "camelCase")]
    SetActivity {
        #[serde(default)]
        last_message_at: Option<i64>,
        #[serde(default)]
        created_at: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    SetHost {
        device_id: String,
    },
    SetConfig {
        config: ChatConfig,
    },
    /// Explicit same-provider, next-safe-turn model/config change.
    SetModel {
        config: ChatConfig,
    },
    CompactContext {},
    ClearContext {},
    SwitchProvider {
        config: ChatConfig,
        #[serde(default)]
        handoff: Option<ContextHandoff>,
    },
    #[serde(rename_all = "camelCase")]
    SetPermission {
        mode: PermissionMode,
        #[serde(default)]
        actor: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SetProviderAccount {
        account_id: String,
    },
    #[serde(rename_all = "camelCase")]
    QueueMessage {
        message_id: String,
        text: String,
        #[serde(default)]
        attachments: Vec<String>,
        #[serde(default)]
        hold_for_turn_end: bool,
        #[serde(default)]
        follow_up_behavior: FollowUpBehavior,
        #[serde(default)]
        execution: Option<ChatConfig>,
    },
    #[serde(rename_all = "camelCase")]
    EditQueuedMessage {
        message_id: String,
        expected_queue_revision: u64,
        lease_id: String,
        text: String,
        expected_text_hash: String,
        #[serde(default)]
        attachments: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    RemoveQueuedMessage {
        message_id: String,
        expected_queue_revision: u64,
    },
    #[serde(rename_all = "camelCase")]
    SendQueuedNow {
        message_id: String,
        #[serde(default)]
        expected_queue_revision: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    SteerQueuedNow {
        message_id: String,
        #[serde(default)]
        expected_queue_revision: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    RetryRecovery {
        chain_id: String,
    },
    #[serde(rename_all = "camelCase")]
    CancelRecovery {
        chain_id: String,
    },
    #[serde(rename_all = "camelCase")]
    MarkSeen {
        #[serde(default)]
        at: Option<i64>,
    },
}

#[derive(Debug, Clone)]
struct ValidatedProjectMove {
    destination: zeron_proto::Space,
}

fn commit_validated_project_move(
    doc: &mut RegistryDoc,
    task_id: &str,
    validated: &ValidatedProjectMove,
) -> Result<(), crate::EngineError> {
    let current = doc
        .space(&validated.destination.id)?
        .ok_or_else(|| crate::EngineError::Other("the validated destination disappeared".into()))?;
    if current.device_id != validated.destination.device_id
        || current.path != validated.destination.path
        || current.checkout_id != validated.destination.checkout_id
    {
        return Err(crate::EngineError::Other(
            "the destination topology changed after validation".into(),
        ));
    }
    if !doc.set_chat_space(task_id, &validated.destination.id)? {
        return Err(crate::EngineError::Other(
            "the task or validated destination changed before commit".into(),
        ));
    }
    Ok(())
}

impl TaskCommandIntent {
    fn name(&self) -> &'static str {
        match self {
            Self::CreateChat { .. } => "createChat",
            Self::Session { command, .. } => match command {
                SessionCommandPayload::Run { .. } => "run",
                SessionCommandPayload::Steer { .. } => "steer",
                SessionCommandPayload::Interrupt {} => "stop",
                SessionCommandPayload::RespondInput { .. } => "respondInput",
                SessionCommandPayload::RespondAsyncQuestion { .. } => "respondAsyncQuestion",
                SessionCommandPayload::RespondApproval { .. } => "respondApproval",
            },
            Self::Rename { .. } => "rename",
            Self::SetPinned { .. } => "setPinned",
            Self::SetRead { .. } => "setRead",
            Self::SetArchived { .. } => "setArchived",
            Self::MoveProject { .. } => "moveProject",
            Self::ForkTask { .. } => "forkTask",
            Self::EditEarlierMessage { .. } => "editEarlierMessage",
            Self::RestoreHistoryRecovery { .. } => "restoreHistoryRecovery",
            Self::Delete {} => "delete",
            Self::SetBranch { .. } => "setBranch",
            Self::SetCwd { .. } => "setCwd",
            Self::SetActivity { .. } => "setActivity",
            Self::SetHost { .. } => "setHost",
            Self::SetConfig { .. } => "setConfig",
            Self::SetModel { .. } => "setModel",
            Self::CompactContext {} => "compactContext",
            Self::ClearContext {} => "clearContext",
            Self::SwitchProvider { .. } => "switchProvider",
            Self::SetPermission { .. } => "setPermission",
            Self::SetProviderAccount { .. } => "setProviderAccount",
            Self::QueueMessage { .. } => "queueMessage",
            Self::EditQueuedMessage { .. } => "editQueuedMessage",
            Self::RemoveQueuedMessage { .. } => "removeQueuedMessage",
            Self::SendQueuedNow { .. } => "sendQueuedNow",
            Self::SteerQueuedNow { .. } => "steerQueuedNow",
            Self::RetryRecovery { .. } => "retryRecovery",
            Self::CancelRecovery { .. } => "cancelRecovery",
            Self::MarkSeen { .. } => "markSeen",
        }
    }

    fn requires_revision(&self) -> bool {
        matches!(
            self,
            Self::Rename { .. }
                | Self::SetPinned { .. }
                | Self::SetRead { .. }
                | Self::SetArchived { .. }
                | Self::MoveProject { .. }
                | Self::ForkTask { .. }
                | Self::EditEarlierMessage { .. }
                | Self::RestoreHistoryRecovery { .. }
                | Self::Delete {}
                | Self::SetBranch { .. }
                | Self::SetCwd { .. }
                | Self::SetHost { .. }
                | Self::SetConfig { .. }
                | Self::SetModel { .. }
                | Self::CompactContext {}
                | Self::ClearContext {}
                | Self::SwitchProvider { .. }
                | Self::SetPermission { .. }
                | Self::SetProviderAccount { .. }
                | Self::MarkSeen { .. }
        )
    }

    fn requires_turn(&self) -> bool {
        matches!(
            self,
            Self::Session {
                command: SessionCommandPayload::Steer { .. } | SessionCommandPayload::Interrupt {},
                ..
            } | Self::SteerQueuedNow { .. }
        )
    }

    fn requires_binding_fence(&self) -> bool {
        !matches!(self, Self::CreateChat { .. } | Self::SetActivity { .. })
    }

    fn conflicts_with_active_turn(&self) -> bool {
        matches!(
            self,
            Self::SetArchived { .. }
                | Self::MoveProject { .. }
                | Self::ForkTask { .. }
                | Self::EditEarlierMessage { .. }
                | Self::RestoreHistoryRecovery { .. }
                | Self::Delete {}
                | Self::SetBranch { .. }
                | Self::SetCwd { .. }
                | Self::SetHost { .. }
                | Self::SetConfig { .. }
                | Self::SetModel { .. }
                | Self::CompactContext {}
                | Self::ClearContext {}
                | Self::SwitchProvider { .. }
        )
    }
}

fn default_actor() -> String {
    "local-user".into()
}

fn default_client() -> String {
    "local-ui".into()
}

fn default_client_session() -> String {
    "legacy".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskCommandReceiptParams {
    command_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskCommandSnapshotParams {
    task_id: String,
    #[serde(default)]
    after_task_sequence: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComposerDraftReadParams {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    draft_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComposerDraftSaveParams {
    draft: ComposerDraft,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeginDraftSubmissionParams {
    command_id: String,
    #[serde(default)]
    task_id: Option<String>,
    snapshot: ComposerDraft,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettleDraftSubmissionParams {
    command_id: String,
    outcome: DraftSubmissionOutcome,
    #[serde(default)]
    queue_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum DraftSubmissionOutcome {
    Pending,
    Recoverable,
    Applied,
    Recovered,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayCommandParams {
    chat_id: String,
    /// The full command entry, client-minted id included — the exactly-once
    /// key the host claims in its processed ledger before executing.
    entry: zeron_doc::SessionCommandEntry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueMessageParams {
    chat_id: String,
    #[serde(default)]
    command_id: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    text: String,
    #[serde(default)]
    attachments: Vec<String>,
    /// Keep this row visible during the current turn even when the harness
    /// supports mid-turn steering.
    #[serde(default)]
    hold_for_turn_end: bool,
    #[serde(default)]
    follow_up_behavior: FollowUpBehavior,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueuedMessageParams {
    chat_id: String,
    id: String,
    /// Present for UpdateQueuedMessage only; empty text deletes the row.
    #[serde(default)]
    text: String,
    /// Present for MoveQueuedMessage only.
    #[serde(default)]
    to_index: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeginQueuedMessageEditParams {
    chat_id: String,
    id: String,
    editor_device_id: String,
    editor_instance_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenewQueuedMessageEditParams {
    chat_id: String,
    id: String,
    lease_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum FinishQueuedMessageEditAction {
    Commit,
    Cancel,
    Discard,
    ReleaseUnchanged,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinishQueuedMessageEditParams {
    chat_id: String,
    id: String,
    lease_id: String,
    action: FinishQueuedMessageEditAction,
    #[serde(default)]
    command_id: Option<String>,
    #[serde(default)]
    expected_queue_revision: Option<u64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    expected_text_hash: Option<String>,
    #[serde(default)]
    attachments: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoPathParams {
    /// `repoPath` per §3.5 (the §2.1 shorthand `repo` is accepted as an alias).
    #[serde(alias = "repo")]
    repo_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckoutChangeRequestParams {
    cwd: String,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchRefParams {
    /// The checkout to switch — a session's cwd (main folder or worktree).
    repo_path: String,
    ref_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWorktreeParams {
    #[serde(alias = "repo")]
    repo_path: String,
    branch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteWorktreeParams {
    #[serde(alias = "repo")]
    repo_path: String,
    #[serde(alias = "path")]
    worktree_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFoldersParams {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileSearchParams {
    query: String,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    space_id: Option<String>,
    /// Existing linked worktree selected for a new chat. The engine accepts it
    /// only after verifying it against the space repository's worktree list.
    #[serde(default)]
    path: Option<String>,
}

fn tool_file_path(call: &ToolCall) -> Option<&str> {
    match call {
        ToolCall::ReadFile { path }
        | ToolCall::WriteFile { path, .. }
        | ToolCall::EditFile { path, .. } => Some(path),
        ToolCall::ApplyPatch { path } | ToolCall::Search { path, .. } => path.as_deref(),
        ToolCall::Exec { .. }
        | ToolCall::Glob { .. }
        | ToolCall::WebFetch { .. }
        | ToolCall::WebSearch { .. }
        | ToolCall::Todo { .. }
        | ToolCall::Mcp { .. }
        | ToolCall::Unknown { .. } => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenTerminalParams {
    chat_id: String,
    cols: u16,
    rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalIdParams {
    terminal_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListTerminalsParams {
    #[serde(default)]
    chat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscribeTerminalParams {
    terminal_id: String,
    #[serde(default)]
    after_seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteTerminalParams {
    terminal_id: String,
    /// Base64 input bytes (plain UTF-8 accepted leniently).
    data: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResizeTerminalParams {
    terminal_id: String,
    cols: u16,
    rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAgentAccountsParams {
    #[serde(default)]
    force_usage: Option<bool>,
    #[serde(default)]
    harness: Option<HarnessId>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentAccountParams {
    harness: HarnessId,
    account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameAgentAccountParams {
    harness: HarnessId,
    account_id: String,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartAgentLoginParams {
    harness: HarnessId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginIdParams {
    login_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteAgentLoginParams {
    login_id: String,
    code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadChunkParams {
    upload_id: String,
    /// Base64 payload chunk.
    data: String,
    #[serde(default)]
    seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadCommitParams {
    upload_id: String,
    file_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadAttachmentChunkParams {
    path: String,
    #[serde(default)]
    offset: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchToolBlobParams {
    /// Doc-resident sidecar ref (`{chatId}/{partId}` or `…​.diff`).
    blob_ref: String,
}

/// The Mutate surface (feature-inventory §2 DataRpc), tagged by `op`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum MutateParams {
    #[serde(rename_all = "camelCase")]
    CreateChat {
        chat_id: String,
        /// The project the chat is created in — fixes host device + base cwd.
        /// `None` mints a project-less chat: `deviceId` picks the host and the
        /// cwd defaults to `~` (expanded on the host at run time).
        #[serde(default)]
        space_id: Option<String>,
        /// Host device for a project-less chat; ignored when `spaceId` is set.
        #[serde(default)]
        device_id: Option<String>,
        #[serde(default)]
        config: Option<ChatConfig>,
        /// The picked ref, named on the row from the first frame (the footer
        /// read "Select ref" until the diff reconciler stamped it).
        #[serde(default)]
        branch: Option<String>,
        /// Cwd override (isolated-worktree path); default = the space's folder.
        #[serde(default)]
        cwd: Option<String>,
        /// Parent hierarchy, fork/copy source, and lifecycle ownership remain
        /// separate even before their later UI surfaces are implemented.
        #[serde(default)]
        lineage: TaskLineage,
    },
    /// Create a space (device + folder pair). Idempotent by id; a live
    /// duplicate `(deviceId, path)` no-ops. `gitDetected` is seeded from the
    /// picker's FolderEntry — the owning device's SpacesSync re-verifies.
    #[serde(rename_all = "camelCase")]
    CreateSpace {
        space_id: String,
        device_id: String,
        path: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        git_detected: bool,
    },
    /// LWW display-name set; `name: None` clears back to basename(path).
    #[serde(rename_all = "camelCase")]
    RenameSpace {
        space_id: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Hard delete: cascades to every chat (and session row) in the space.
    /// Live runs hosted here are interrupted best-effort.
    #[serde(rename_all = "camelCase")]
    DeleteSpace { space_id: String },
    #[serde(rename_all = "camelCase")]
    RenameChat { chat_id: String, title: String },
    /// Set the chat's checkout branch label — the sidebar's
    /// "project · branch" sub-line.
    #[serde(rename_all = "camelCase")]
    SetChatBranch { chat_id: String, branch: String },
    /// Retarget a chat onto another folder — mid-session switch to an
    /// EXISTING worktree (the picked ref's checkout). Next run starts a
    /// fresh harness conversation there (resume is cwd-scoped).
    #[serde(rename_all = "camelCase")]
    SetChatCwd { chat_id: String, cwd: String },
    /// Backdate a chat's activity timestamps (epoch ms) — the sidebar's
    /// relative-time column. Used by tooling/seeds; the doc fold sets these on
    /// real message traffic.
    #[serde(rename_all = "camelCase")]
    SetChatActivity {
        chat_id: String,
        #[serde(default)]
        last_message_at: Option<i64>,
        #[serde(default)]
        created_at: Option<i64>,
    },
    /// Re-home a chat to another device (tooling/seeds; device migration later).
    #[serde(rename_all = "camelCase")]
    SetChatHost { chat_id: String, device_id: String },
    #[serde(rename_all = "camelCase")]
    SetChatArchived { chat_id: String, archived: bool },
    /// Full-config replace on the chat row (zeron `SetChatConfig`): the
    /// composer's mid-session model / reasoning / options changes, LWW-synced
    /// so they survive restarts and reach every device.
    #[serde(rename_all = "camelCase")]
    SetChatConfig { chat_id: String, config: ChatConfig },
    /// Engine-owned permission mutation: stages the requested state, preserves
    /// an active provider's effective state, and publishes the resolved state
    /// only at a safe runtime boundary. Both transitions are audited.
    #[serde(rename_all = "camelCase")]
    SetThreadPermissionMode {
        chat_id: String,
        mode: PermissionMode,
        #[serde(default)]
        actor: Option<String>,
    },
    /// Engine-owned provider-account transition. Rejects active turns and
    /// retires an idle warm runtime before publishing the new chat binding.
    #[serde(rename_all = "camelCase")]
    SetThreadProviderAccount { chat_id: String, account_id: String },
    /// Tombstone: removes the chats-map row; the session doc remains.
    #[serde(rename_all = "camelCase")]
    DeleteChat { chat_id: String },
    #[serde(rename_all = "camelCase")]
    RenameDevice { device_id: String, name: String },
    /// Synced seen marker (LWW + monotonic guard): clears the "completed"
    /// badge on every device. `at` is epoch ms; default = now.
    #[serde(rename_all = "camelCase")]
    MarkChatSeen {
        chat_id: String,
        #[serde(default)]
        at: Option<i64>,
    },
}

pub struct EngineRpc {
    backup_control: Option<BackupControl>,
    store: std::sync::Arc<DocsStore>,
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    registry: std::sync::Arc<HarnessRegistry>,
    repos: Repos,
    git_actions: crate::git_actions::GitActions,
    workspace_files: crate::WorkspaceFiles,
    terminals: Terminals,
    previews: Option<zeron_preview::PreviewService>,
    change_requests: CheckoutChangeRequests,
    diff_sync: CheckoutDiffSync,
    uploads: Uploads,
    agent_accounts: AgentAccounts,
    task_identity: crate::TaskIdentityService,
    auth: Option<Auth>,
    links: Option<std::sync::Arc<LinkCache>>,
    updater: Option<zeron_update::Updater>,
    local_import: Option<crate::local_import::LocalImporter>,
    engine_info: EngineInfo,
    activity_cancel: CancellationToken,
}

impl Drop for EngineRpc {
    fn drop(&mut self) {
        self.activity_cancel.cancel();
    }
}

impl EngineRpc {
    #[allow(clippy::too_many_arguments)] // engine assembly seam, not a public API
    pub fn new(
        store: std::sync::Arc<DocsStore>,
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        registry: std::sync::Arc<HarnessRegistry>,
        repos: Repos,
        workspace_files: crate::WorkspaceFiles,
        terminals: Terminals,
        change_requests: CheckoutChangeRequests,
        diff_sync: CheckoutDiffSync,
        uploads: Uploads,
        agent_accounts: AgentAccounts,
        task_identity: crate::TaskIdentityService,
        workspace_scope: WorkspaceScope,
    ) -> Self {
        let engine_info = EngineInfo {
            device_id: doc_host.device_id().to_string(),
            workspace_scope,
            capabilities: zeron_proto::capabilities::current(),
        };
        let rpc = Self {
            backup_control: None,
            store,
            sessions,
            doc_host,
            workspace,
            registry,
            git_actions: crate::git_actions::GitActions::new(repos.clone()),
            repos,
            workspace_files,
            terminals,
            previews: None,
            change_requests,
            diff_sync,
            uploads,
            agent_accounts,
            task_identity,
            auth: None,
            links: None,
            updater: None,
            local_import: None,
            engine_info,
            activity_cancel: CancellationToken::new(),
        };
        rpc.spawn_activity_bootstrap();
        rpc
    }

    /// Migration/reopen ingestion plus metadata reconciliation. This runs
    /// independently of search, so showing results never fetches sensitive
    /// source payload on demand. Only tasks whose index-relevant workspace
    /// metadata changed are reread; deleted tasks lose their derived rows.
    fn spawn_activity_bootstrap(&self) {
        let store = self.store.clone();
        let doc_host = self.doc_host.clone();
        let workspace = self.workspace.clone();
        let identity = self.task_identity.clone();
        let sessions = self.sessions.clone();
        let local_device_id = self.engine_info.device_id.clone();
        let cancel = self.activity_cancel.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::debug!("session history bootstrap deferred: no Tokio runtime");
            return;
        };
        runtime.spawn(async move {
            let mut chats = workspace.watch_chats();
            let mut indexed = HashMap::<String, String>::new();
            let mut opening = true;
            loop {
                let current = chats.borrow_and_update().clone();
                let current_ids = current
                    .iter()
                    .map(|chat| chat.id.clone())
                    .collect::<HashSet<_>>();
                let known_task_ids = if opening {
                    opening = false;
                    store.activity_task_ids().unwrap_or_else(|error| {
                        tracing::warn!(%error, "session history could not inventory prior task rows");
                        Vec::new()
                    })
                } else {
                    indexed.keys().cloned().collect()
                };
                let deleted = known_task_ids
                    .iter()
                    .filter(|task_id| !current_ids.contains(*task_id))
                    .cloned()
                    .collect::<Vec<_>>();
                for task_id in deleted {
                    if let Err(error) = store.replace_activity_events(&task_id, &[]) {
                        tracing::warn!(task = %task_id, %error, "session history could not remove deleted task rows");
                    } else {
                        indexed.remove(&task_id);
                    }
                }
                for chat in current {
                    let fingerprint = activity_metadata_fingerprint(&chat);
                    if indexed.get(&chat.id) == Some(&fingerprint) {
                        continue;
                    }
                    match doc_host.open(&chat.id) {
                        Ok(handle) => match handle.doc().read_entries() {
                            Ok(entries) => {
                                if let Err(error) = index_activity_task(
                                    &store,
                                    &identity,
                                    &sessions,
                                    &local_device_id,
                                    &chat,
                                    entries,
                                ) {
                                    tracing::warn!(task = %chat.id, %error, "session history bootstrap skipped task");
                                } else {
                                    indexed.insert(chat.id.clone(), fingerprint);
                                }
                            }
                            Err(error) => {
                                tracing::warn!(task = %chat.id, %error, "session history bootstrap preserved prior index after source read failure");
                            }
                        },
                        Err(error) => {
                            tracing::warn!(task = %chat.id, %error, "session history bootstrap could not open task");
                        }
                    }
                    tokio::task::yield_now().await;
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = chats.changed() => if changed.is_err() { break },
                }
            }
        });
    }

    pub fn with_previews(mut self, previews: zeron_preview::PreviewService) -> Self {
        self.previews = Some(previews);
        self
    }

    /// Attach the auth service (AuthStatus + AuthRpc mutations).
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// The scheduler reads only a separately marked copy-isolated source. A
    /// normal LUNA root never constructs this control or starts a backup job.
    pub fn with_backup_control(mut self, control: BackupControl) -> Self {
        let scheduler = control.clone();
        let cancel = self.activity_cancel.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let control = scheduler.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                if let Err(error) = control.tick_automatic(crate::now_ms()) {
                                    let message = error.to_string();
                                    let _ = control.record_scheduler_error(message);
                                }
                            }).await;
                        }
                    }
                }
            });
        }
        self.backup_control = Some(control);
        self
    }

    /// Attach the peer link cache — enables `targetDeviceId` relay forwarding.
    pub fn with_links(mut self, links: std::sync::Arc<LinkCache>) -> Self {
        self.links = Some(links);
        self
    }

    /// Attach the release checker (UpdateStatus stream + ApplyUpdate).
    pub fn with_updater(mut self, updater: zeron_update::Updater) -> Self {
        self.updater = Some(updater);
        self
    }

    /// Attach the local→synced profile importer (synced runtimes only).
    pub fn with_local_import(mut self, importer: crate::local_import::LocalImporter) -> Self {
        self.local_import = Some(importer);
        self
    }

    fn auth(&self) -> Result<&Auth, RpcError> {
        self.auth
            .as_ref()
            .ok_or_else(|| RpcError::Failed("auth unavailable".into()))
    }

    fn backup_control(&self) -> Result<BackupControl, RpcError> {
        self.backup_control.clone().ok_or_else(|| RpcError::Failed(
            "Backup controls require an explicitly marked copy-isolated LUNA environment. Live profile backup is unavailable.".into()
        ))
    }

    fn updater(&self) -> Result<&zeron_update::Updater, RpcError> {
        self.updater
            .as_ref()
            .ok_or_else(|| RpcError::Failed("updates unavailable".into()))
    }

    fn local_importer(&self) -> Result<&crate::local_import::LocalImporter, RpcError> {
        self.local_import
            .as_ref()
            .ok_or_else(|| RpcError::Failed("local import requires a synced workspace".into()))
    }

    fn ensure_local_attention_target(&self, target_device_id: &str) -> Result<(), RpcError> {
        if target_device_id != self.engine_info.device_id {
            return Err(RpcError::Failed(
                "Activity for another environment is unavailable until scoped remote access is implemented."
                    .into(),
            ));
        }
        Ok(())
    }

    fn reconcile_attention(&self) -> Result<AttentionSnapshot, RpcError> {
        let chats = self
            .workspace
            .read_chats()
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        // The live session map is process-local. Settled status and completion
        // markers are mirrored into the workspace doc, so a restart must use
        // those rows until this engine emits another transition.
        let stored_sessions = self
            .workspace
            .read_sessions()
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let mut desired = Vec::new();
        let connectivity = self.doc_host.watch_connectivity();
        if let Some(item) = crate::attention::project_connectivity(
            &self.engine_info.device_id,
            connectivity.borrow().state,
            crate::now_ms(),
        ) {
            desired.push(item);
        }
        for chat in chats
            .iter()
            .filter(|chat| chat.device_id == self.engine_info.device_id)
        {
            let handle = self
                .doc_host
                .open(&chat.id)
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            let entries = handle
                .doc()
                .read_entries()
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            let queue = handle
                .doc()
                .read_queue()
                .map_err(|error| RpcError::Failed(error.to_string()))?;
            let session = self.sessions.session_status(&chat.id).or_else(|| {
                stored_sessions
                    .iter()
                    .find(|session| {
                        session.chat_id == chat.id
                            && session.device_id == self.engine_info.device_id
                    })
                    .cloned()
            });
            desired.extend(crate::attention::project_task(
                &self.engine_info.device_id,
                chat,
                session.as_ref(),
                &entries,
                &queue,
            ));
        }
        self.store
            .reconcile_attention_items(&desired, crate::attention::MANAGED_KINDS)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let stored = self
            .store
            .attention_snapshot()
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        Ok(AttentionSnapshot {
            contract_version: zeron_proto::ATTENTION_CONTRACT_VERSION,
            revision: stored.revision,
            read_at_ms: crate::now_ms(),
            unresolved_count: stored.unresolved_count,
            needs_attention: stored.needs_attention,
            recent: stored.recent,
            producers: vec![
                AttentionProducerAvailability { source: "local task requests, run/recovery state, queue, and edge connectivity".into(), available: true, reason: None },
                AttentionProducerAvailability { source: "remote host and device grants".into(), available: false, reason: Some("Scoped remote authorization is not implemented.".into()) },
                AttentionProducerAvailability { source: "automations".into(), available: false, reason: Some("Automations are a later roadmap feature.".into()) },
                AttentionProducerAvailability { source: "engine/CLI compatibility blockers".into(), available: false, reason: Some("A stable operation-blocking compatibility producer is not available.".into()) },
                AttentionProducerAvailability { source: "provider account health".into(), available: false, reason: Some("Only a task's authoritative recovery sign-in state is available; no provider health probe is performed.".into()) },
            ],
        })
    }

    fn validate_composer_draft(&self, draft: &ComposerDraft) -> Result<(), RpcError> {
        if draft.version != zeron_proto::COMPOSER_DRAFT_VERSION {
            return Err(RpcError::BadParams(
                "unsupported composer draft version".into(),
            ));
        }
        if draft.draft_id.is_empty() || draft.draft_id.len() > 128 {
            return Err(RpcError::BadParams("invalid composer draft id".into()));
        }
        let encoded =
            serde_json::to_vec(draft).map_err(|error| RpcError::BadParams(error.to_string()))?;
        if encoded.len() > 4 * 1024 * 1024 {
            return Err(RpcError::BadParams(
                "composer draft metadata is too large".into(),
            ));
        }
        for blob in &draft.attachments {
            if blob.state != DraftBlobState::Saved {
                continue;
            }
            let path = blob
                .path
                .as_deref()
                .ok_or_else(|| RpcError::BadParams("saved draft attachment has no path".into()))?;
            self.uploads
                .validate_draft_blob(path, blob.size, blob.sha256.as_deref())
                .map_err(|error| RpcError::BadParams(error.to_string()))?;
        }
        Ok(())
    }

    fn mark_unavailable_draft_blobs(&self, draft: &mut ComposerDraft) {
        for blob in &mut draft.attachments {
            let validation = match blob.state {
                DraftBlobState::Saved => blob
                    .path
                    .as_deref()
                    .ok_or_else(|| {
                        crate::EngineError::Other("Saved attachment has no durable path".into())
                    })
                    .and_then(|path| {
                        self.uploads
                            .validate_draft_blob(path, blob.size, blob.sha256.as_deref())
                    }),
                DraftBlobState::WaitingForUpload => Err(crate::EngineError::Other(
                    "Attachment copy did not finish before the app stopped".into(),
                )),
                DraftBlobState::NeedsReattachment => continue,
            };
            if let Err(error) = validation {
                blob.state = DraftBlobState::NeedsReattachment;
                blob.error = Some(error.to_string());
                blob.path = None;
            }
        }
    }

    fn read_composer_draft(
        &self,
        task_id: Option<&str>,
        draft_id: Option<&str>,
    ) -> Result<ComposerDraftRead, RpcError> {
        let mut read = self
            .store
            .read_composer_draft(&self.engine_info.device_id, task_id, draft_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        if let Some(draft) = &mut read.draft {
            self.mark_unavailable_draft_blobs(draft);
        }
        for submission in &mut read.submissions {
            self.mark_unavailable_draft_blobs(&mut submission.snapshot);
        }
        Ok(read)
    }

    /// Resolve a mention-search root from synced workspace rows. A client may
    /// name an existing linked worktree for a new chat, but it is verified
    /// against the space repository before any filesystem walk begins.
    async fn file_search_root(&self, p: &FileSearchParams) -> Result<std::path::PathBuf, RpcError> {
        let target = zeron_proto::WorkspaceTarget {
            chat_id: p.chat_id.clone(),
            space_id: p.space_id.clone(),
            checkout_path: p.path.clone(),
        };
        self.workspace_files
            .resolve_target(&target)
            .await
            .map(|workspace| workspace.root)
            .map_err(Into::into)
    }

    /// Accept only a checkout already named by a local chat or contained in a
    /// local space. Remote clients must not turn this RPC into an arbitrary path probe.
    async fn change_request_root(&self, cwd: &str) -> Result<std::path::PathBuf, RpcError> {
        let requested = std::path::PathBuf::from(cwd);
        let local_device = self.doc_host.device_id();
        let mut chats_rx = self.workspace.watch_chats();
        let mut spaces_rx = self.workspace.watch_spaces();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
        loop {
            let chats = chats_rx.borrow_and_update().clone();
            if chats.iter().any(|chat| {
                chat.device_id == local_device
                    && chat.cwd.as_deref().map(std::path::Path::new) == Some(requested.as_path())
            }) {
                return Ok(requested);
            }

            let spaces = spaces_rx.borrow_and_update().clone();
            for space in spaces
                .iter()
                .filter(|space| space.device_id == local_device)
            {
                if let Some(checkout) = self
                    .repos
                    .workspace_checkout(std::path::Path::new(&space.path), &requested)
                    .await
                {
                    return Ok(checkout);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                _ = chats_rx.changed() => {}
                _ = spaces_rx.changed() => {}
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        Err(RpcError::BadParams(
            "cwd is not a known checkout on this device".into(),
        ))
    }

    /// Most-recent-first paths the current chat actually touched, followed by
    /// files still changed in its checkout. The search worker validates and
    /// normalizes them against the resolved root before using them as ranking
    /// hints, so stale or out-of-workspace tool paths simply disappear.
    fn featured_file_paths(&self, chat_id: &str) -> Vec<String> {
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        if let Ok(handle) = self.doc_host.open(chat_id)
            && let Ok(entries) = handle.doc().read_entries()
        {
            for entry in entries.into_iter().rev() {
                for part in entry.parts.into_iter().rev() {
                    if let MessagePart::Tool { call, .. } = part
                        && let Some(path) = tool_file_path(&call)
                        && !path.trim().is_empty()
                        && seen.insert(path.to_string())
                    {
                        paths.push(path.to_string());
                        if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                            break;
                        }
                    }
                }
                if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                    break;
                }
            }
        }

        if let Ok(Some(chat)) = self.workspace.chat(chat_id) {
            let diffs = self.diff_sync.watch_diffs().borrow().clone();
            let diff = chat
                .checkout_id
                .as_deref()
                .and_then(|id| diffs.iter().find(|diff| diff.checkout_id == id))
                .or_else(|| {
                    chat.cwd
                        .as_deref()
                        .and_then(|cwd| diffs.iter().find(|diff| diff.cwd == cwd))
                });
            if let Some(diff) = diff {
                for file in &diff.files {
                    if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                        break;
                    }
                    if seen.insert(file.path.clone()) {
                        paths.push(file.path.clone());
                    }
                }
            }
        }
        paths
    }

    /// Forward a device-addressed call over the target device's relay. On transport
    /// failure the cached link is invalidated so the next call re-dials.
    async fn forward(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let Some(links) = &self.links else {
            return Err(RpcError::Failed(format!(
                "cannot reach device {target}: remote routing unavailable (offline)"
            )));
        };
        let client = links.client(target).await?;
        if is_stream_method(method) {
            // Streams are unbounded by design (a quiet WATCH_* is healthy);
            // only unary calls below get the reply deadline.
            if method == methods::WATCH_CHECKOUT_CHANGE_REQUEST {
                let rx = match client.subscribe_checked(method, params).await {
                    Ok(rx) => rx,
                    Err(err) => {
                        if should_invalidate_link(&err) {
                            links.invalidate(target);
                        }
                        return Err(err);
                    }
                };
                let stream = futures::stream::unfold((rx, client), |(mut rx, client)| async move {
                    rx.recv().await.map(|item| (item, (rx, client)))
                });
                return Ok(RpcReply::Stream(stream.boxed()));
            }
            let rx = match client.subscribe(method, params).await {
                Ok(rx) => rx,
                Err(err) => {
                    if should_invalidate_link(&err) {
                        links.invalidate(target);
                    }
                    return Err(err);
                }
            };
            // Pipe remote items; the held client keeps the link's RpcClient alive for
            // the stream's lifetime. A remote error just ends the stream (the relay
            // link-down path fails pending calls; stream receivers close).
            let stream = futures::stream::unfold((rx, client), |(mut rx, client)| async move {
                rx.recv().await.map(|item| (item, (rx, client)))
            });
            return Ok(RpcReply::Stream(stream.boxed()));
        }
        let deadline = forward_deadline(method);
        match tokio::time::timeout(deadline, client.call(method, params)).await {
            Ok(Ok(value)) => Ok(RpcReply::Value(value)),
            Ok(Err(err)) => {
                if should_invalidate_link(&err) {
                    links.invalidate(target);
                }
                Err(err)
            }
            Err(_) => {
                // No reply inside the deadline. The link may be a zombie — the
                // relay's auto-pong keeps a dead host socket looking alive
                // (ws3 auto-pong incident) — so drop it; the next call re-dials.
                // NOTE: the remote may still complete the forwarded work; the
                // caller sees a retryable failure instead of hanging forever
                // (the "Sending…" wedge, 2026-08-18).
                links.invalidate(target);
                Err(RpcError::Transport(format!(
                    "no reply from device {target} for {method} within {}s",
                    deadline.as_secs()
                )))
            }
        }
    }

    async fn mutate(&self, params: MutateParams) -> Result<(), RpcError> {
        let failed = |e: crate::EngineError| RpcError::Failed(e.to_string());
        match params {
            MutateParams::CreateSpace {
                space_id,
                device_id,
                path,
                name,
                git_detected,
            } => self
                .workspace
                .create_space(&space_id, &device_id, &path, name, git_detected)
                .map_err(failed),
            MutateParams::RenameSpace { space_id, name } => self
                .workspace
                .rename_space(&space_id, name.as_deref())
                .map_err(failed)
                .map(drop),
            MutateParams::DeleteSpace { space_id } => {
                let deleted = self.workspace.delete_space(&space_id).map_err(failed)?;
                // Best-effort teardown of live runs we host for the deleted chats
                // (the doc rows are already tombstoned; a straggler run would only
                // write into an orphaned session doc).
                let sessions = self.sessions.clone();
                let doc_host = self.doc_host.clone();
                let chat_ids = deleted.chat_ids;
                for chat_id in &chat_ids {
                    self.task_identity.tombstone_task(chat_id).map_err(failed)?;
                    let removed = self.terminals.delete_task(chat_id).map_err(failed)?;
                    if removed > 0 {
                        tracing::info!(chat = %chat_id, records = removed, "deleted task terminal history");
                    }
                }
                tokio::spawn(async move {
                    for chat_id in chat_ids {
                        if let Err(err) = sessions.interrupt(&chat_id).await {
                            tracing::debug!(chat = %chat_id, error = %err, "deleteSpace interrupt skipped");
                        }
                        doc_host.purge_chat(&chat_id);
                    }
                });
                Ok(())
            }
            MutateParams::RenameDevice { device_id, name } => self
                .workspace
                .rename_device(&device_id, &name)
                .map_err(failed)
                .map(drop),
            _ => Err(RpcError::BadParams(
                "task mutations must use ExecuteTaskCommand".into(),
            )),
        }
    }

    fn current_task_command_snapshot(
        &self,
        task_id: &str,
    ) -> Result<TaskCommandSnapshot, RpcError> {
        let mut snapshot = self
            .doc_host
            .task_command_snapshot(task_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .snapshot;
        if let Some(binding) = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding)
        {
            snapshot.binding_id = Some(binding.binding_id);
            snapshot.binding_generation = Some(binding.generation);
        }
        snapshot.active_turn_id = self.sessions.active_turn_id(task_id);
        Ok(snapshot)
    }

    async fn execute_task_command(
        &self,
        mut command: TaskCommandParams,
        legacy: bool,
    ) -> Result<CommandReceipt, RpcError> {
        let _lane = self.doc_host.task_command_gate(&command.task_id).await;
        let current = self.current_task_command_snapshot(&command.task_id)?;
        if legacy {
            if command.intent.requires_revision() {
                command.expected_task_revision = Some(current.task_revision);
            }
            command.expected_binding_generation = current.binding_generation;
            if command.intent.requires_turn()
                || (matches!(&command.intent, TaskCommandIntent::SendQueuedNow { .. })
                    && current.active_turn_id.is_some())
            {
                command.expected_turn_id = current.active_turn_id.clone();
            }
        }
        let payload_hash = canonical_payload_hash(&command.intent)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let now = crate::now_ms();
        let pending = CommandReceipt {
            command_id: command.command_id.clone(),
            task_id: command.task_id.clone(),
            intent: command.intent.name().into(),
            payload_hash,
            status: CommandReceiptStatus::Pending,
            snapshot: current.clone(),
            event_sequence: None,
            actor_id: command.actor_id.clone(),
            client_id: command.client_id.clone(),
            client_session_id: command.client_session_id.clone(),
            execution_environment: Some(self.engine_info.device_id.clone()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: command.issued_at_ms.min(now),
            committed_at_ms: None,
            observed_at_ms: now,
        };
        let begin = self
            .doc_host
            .begin_task_command(
                pending,
                command.expected_task_revision,
                command.expected_binding_generation,
                command.expected_turn_id.as_deref(),
            )
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        match begin.kind {
            zeron_sync::TaskCommandBeginKind::Duplicate => {
                if begin.receipt.status != CommandReceiptStatus::Pending
                    || begin.receipt.reason.is_some()
                    || !self.safe_local_projection_retry(&command.task_id, &command.intent)
                {
                    return Ok(begin.receipt);
                }
                if begin.receipt.snapshot.task_revision != current.task_revision {
                    return self.finish_task_command(
                        &command.command_id,
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::StaleTaskRevision,
                        "the task changed before the pending local projection could be retried",
                        "Reload the task and issue a new command id if the mutation is still needed.",
                    );
                }
            }
            zeron_sync::TaskCommandBeginKind::Conflict => {
                self.doc_host.publish_task_receipt(begin.receipt.clone());
                return Ok(begin.receipt);
            }
            zeron_sync::TaskCommandBeginKind::Accepted => {
                self.doc_host.publish_task_receipt(begin.receipt.clone());
            }
        }

        let needs_turn_fence = command.intent.requires_turn()
            || (matches!(&command.intent, TaskCommandIntent::SendQueuedNow { .. })
                && current.active_turn_id.is_some());
        let missing_fence = if !legacy
            && command.intent.requires_revision()
            && command.expected_task_revision.is_none()
        {
            Some("this mutation requires expectedTaskRevision")
        } else if !legacy && needs_turn_fence && command.expected_turn_id.is_none() {
            Some("this control requires expectedTurnId")
        } else if !legacy
            && command.intent.requires_binding_fence()
            && current.binding_generation.is_some()
            && command.expected_binding_generation.is_none()
        {
            Some("this provider-bound task requires expectedBindingGeneration")
        } else {
            None
        };
        if let Some(message) = missing_fence {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::UnsupportedOperation,
                message,
                "Reload the task command snapshot and retry with a new command id.",
            );
        }
        if command.intent.conflicts_with_active_turn()
            && self.sessions.active_turn_id(&command.task_id).is_some()
        {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::ActiveTurnConflict,
                "the task has an active turn",
                "Stop the expected active turn, wait for settlement, then retry with a new command id.",
            );
        }
        let validated_project_move = if let TaskCommandIntent::MoveProject {
            destination_space_id,
        } = &command.intent
        {
            match self
                .validate_project_move(&command.task_id, destination_space_id)
                .await
            {
                Ok(validated) => Some(validated),
                Err((code, message, recovery)) => {
                    return self.finish_task_command(
                        &command.command_id,
                        CommandReceiptStatus::Rejected,
                        false,
                        code,
                        &message,
                        &recovery,
                    );
                }
            }
        } else {
            None
        };
        let revalidated = self.current_task_command_snapshot(&command.task_id)?;
        if command.expected_binding_generation.is_some()
            && command.expected_binding_generation != revalidated.binding_generation
        {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::StaleBindingGeneration,
                "the provider binding changed after command acceptance",
                "Reload task identity; do not replay this command against the replacement binding.",
            );
        }
        if command.expected_turn_id.is_some()
            && command.expected_turn_id != revalidated.active_turn_id
        {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::StaleTurn,
                "the active turn changed after command acceptance",
                "Reload active task state; do not replay this control against the replacement turn.",
            );
        }
        if validated_project_move.is_some() && self.sessions.has_live_run(&command.task_id) {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::ActiveTurnConflict,
                "the task became active while its project move was being validated",
                "Stop the active turn or pending interaction, wait for settlement, and retry with a new command id.",
            );
        }

        if let TaskCommandIntent::SteerQueuedNow { message_id, .. } = &command.intent {
            let Some(item) = self
                .doc_host
                .queued_message(&command.task_id, message_id)
                .map_err(|error| RpcError::Failed(error.to_string()))?
            else {
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Conflict,
                    false,
                    CommandReasonCode::QueueMissing,
                    "the queued message no longer exists",
                    "Reload the queue; do not create a replacement automatically.",
                );
            };
            if item.delivery_gate.is_some()
                || !matches!(item.status, zeron_doc::QueueMessageStatus::Held)
            {
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Conflict,
                    false,
                    CommandReasonCode::QueueClaimed,
                    "the queued message is being edited, reviewed, or delivered",
                    "Finish the current queue action and reload before retrying.",
                );
            }
            let harness = item
                .execution
                .as_ref()
                .map(|execution| execution.harness)
                .unwrap_or_else(|| self.doc_host.harness_for(&command.task_id));
            if !item.attachments.is_empty() || !self.sessions.steers_mid_turn(harness) {
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Rejected,
                    false,
                    CommandReasonCode::UnsupportedOperation,
                    "the selected agent cannot steer this queued message mid-turn",
                    "Leave it queued or use Send now, which explicitly interrupts the turn.",
                );
            }
        }
        if let TaskCommandIntent::SendQueuedNow { .. } = &command.intent
            && self
                .doc_host
                .request_from_chat_row(&command.task_id, "")
                .is_none()
            && self.sessions.last_request(&command.task_id).is_none()
        {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Rejected,
                false,
                CommandReasonCode::OperationRejected,
                "no durable execution configuration is available for this queued message",
                "Restore the task execution settings before trying Send now again.",
            );
        }

        if let TaskCommandIntent::Session {
            command: mut payload,
            transfers,
        } = command.intent
        {
            let stop_pause_before = if matches!(&payload, SessionCommandPayload::Interrupt {})
                && command.expected_turn_id.is_some()
            {
                Some(
                    self.doc_host
                        .fence_queue_for_stop(&command.task_id)
                        .await
                        .map_err(|error| RpcError::Failed(error.to_string()))?,
                )
            } else {
                None
            };
            if let SessionCommandPayload::Steer { message_id, .. } = &mut payload
                && message_id.is_none()
            {
                *message_id = Some(command.command_id.clone());
            }
            let correlation = match &payload {
                SessionCommandPayload::Run { message_id, .. } => Some(message_id.as_str()),
                SessionCommandPayload::Steer {
                    message_id: Some(message_id),
                    ..
                } => Some(message_id.as_str()),
                _ => None,
            };
            if let Some(correlation) = correlation {
                let superseded = self
                    .sessions
                    .prepare_supersede_recovery(
                        &command.task_id,
                        "Recovery cancelled because newer user activity was accepted.",
                    )
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                if let Some(change) = superseded.as_ref() {
                    let transition = change
                        .storage_transition()
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    self.doc_host
                        .set_task_command_correlation_with_recovery(
                            &command.command_id,
                            correlation,
                            &transition,
                        )
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    self.sessions.publish_committed_recovery(&change.next);
                } else {
                    self.doc_host
                        .set_task_command_correlation(&command.command_id, correlation)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                }
            }
            if let Err(error) = self.doc_host.queue_command_with_id_and_transfers(
                &command.task_id,
                &command.command_id,
                payload,
                transfers,
            ) {
                if let Some(was_paused) = stop_pause_before {
                    let _ = self
                        .doc_host
                        .restore_queue_pause_after_failed_stop(&command.task_id, was_paused);
                }
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Failed,
                    false,
                    CommandReasonCode::OperationFailed,
                    &error.to_string(),
                    "Fix the local validation or storage failure and retry with a new command id.",
                );
            }
            // The durable session-doc executor owns finalization. It may have
            // completed before this read; otherwise the honest result is pending.
            return self
                .doc_host
                .task_command_receipt(&command.command_id)
                .map_err(|error| RpcError::Failed(error.to_string()))?
                .ok_or_else(|| RpcError::Failed("task command receipt vanished".into()));
        }

        let provider_effect = self
            .task_identity
            .has_native_codex_binding(&command.task_id)
            && matches!(
                command.intent,
                TaskCommandIntent::Rename { .. } | TaskCommandIntent::SetArchived { .. }
            )
            || matches!(
                command.intent,
                TaskCommandIntent::SendQueuedNow { .. } | TaskCommandIntent::SteerQueuedNow { .. }
            );
        let provider_correlation = match &command.intent {
            TaskCommandIntent::SendQueuedNow { message_id, .. }
            | TaskCommandIntent::SteerQueuedNow { message_id, .. } => Some(message_id.clone()),
            _ => None,
        };
        // Queue delivery writes correlation and the durable claimed queue
        // projection in one transaction inside DocHost. Publishing the
        // correlation here would recreate a crash window where a receipt
        // names provider work that was never durably claimed.
        let expected_turn_id = command.expected_turn_id.clone();
        let outcome = self
            .apply_task_command_intent(
                &command.command_id,
                &command.task_id,
                command.intent,
                expected_turn_id.as_deref(),
                validated_project_move,
            )
            .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                if provider_effect || matches!(&error, TaskProjectionError::ProviderUnknown(_)) {
                    return self
                        .doc_host
                        .mark_task_command_unknown(
                            &command.command_id,
                            &format!(
                                "provider mutation returned without a provable final state: {}",
                                error.message()
                            ),
                        )
                        .map_err(|store| RpcError::Failed(store.to_string()))?
                        .ok_or_else(|| RpcError::Failed("task command receipt vanished".into()));
                }
                return match error {
                    TaskProjectionError::Mutation(error) => self.finish_task_command(
                        &command.command_id,
                        CommandReceiptStatus::Failed,
                        false,
                        CommandReasonCode::OperationFailed,
                        &error.to_string(),
                        "Review the current task state and retry with a new command id if still needed.",
                    ),
                    TaskProjectionError::Commit(error) => Err(RpcError::Failed(format!(
                        "task projection was rolled back before publication: {error}"
                    ))),
                    TaskProjectionError::ProviderUnknown(_) => unreachable!(
                        "provider-unknown errors are finalized before local error handling"
                    ),
                };
            }
        };
        if provider_correlation.is_some() {
            return self
                .doc_host
                .task_command_receipt(&command.command_id)
                .map_err(|error| RpcError::Failed(error.to_string()))?
                .ok_or_else(|| RpcError::Failed("task command receipt vanished".into()));
        }
        let receipt = outcome
            .ok_or_else(|| RpcError::Failed("task command did not produce a receipt".into()))?;
        self.workspace.publish_task_command_projection();
        self.doc_host.publish_task_receipt(receipt.clone());
        Ok(receipt)
    }

    fn finish_task_command(
        &self,
        command_id: &str,
        status: CommandReceiptStatus,
        applied: bool,
        reason_code: CommandReasonCode,
        message: &str,
        recovery: &str,
    ) -> Result<CommandReceipt, RpcError> {
        let prior = self
            .doc_host
            .task_command_receipt(command_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .ok_or_else(|| RpcError::Failed("task command receipt vanished".into()))?;
        let current = self.current_task_command_snapshot(&prior.task_id)?;
        let reason = (!message.is_empty()).then(|| CommandReceiptReason {
            code: reason_code,
            message: message.into(),
        });
        self.doc_host
            .complete_task_command(
                command_id,
                status,
                applied,
                current.binding_id.as_deref(),
                current.binding_generation,
                current.active_turn_id.as_deref(),
                reason,
                (!recovery.is_empty()).then_some(recovery),
            )
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .ok_or_else(|| RpcError::Failed("task command receipt vanished".into()))
    }

    fn safe_local_projection_retry(&self, task_id: &str, intent: &TaskCommandIntent) -> bool {
        match intent {
            TaskCommandIntent::Rename { .. } | TaskCommandIntent::SetArchived { .. } => {
                !self.task_identity.has_native_codex_binding(task_id)
            }
            TaskCommandIntent::Session { .. }
            | TaskCommandIntent::ForkTask { .. }
            | TaskCommandIntent::EditEarlierMessage { .. }
            | TaskCommandIntent::RestoreHistoryRecovery { .. }
            | TaskCommandIntent::SendQueuedNow { .. }
            | TaskCommandIntent::SteerQueuedNow { .. }
            | TaskCommandIntent::RetryRecovery { .. }
            | TaskCommandIntent::CompactContext {} => false,
            _ => true,
        }
    }

    async fn validate_project_move(
        &self,
        task_id: &str,
        destination_space_id: &str,
    ) -> Result<ValidatedProjectMove, (CommandReasonCode, String, String)> {
        let task = self
            .workspace
            .chat(task_id)
            .map_err(|error| {
                (
                    CommandReasonCode::OperationFailed,
                    error.to_string(),
                    "Reload the task and retry after the workspace is readable.".into(),
                )
            })?
            .ok_or_else(|| {
                (
                    CommandReasonCode::InvalidDestination,
                    "the task no longer exists".into(),
                    "Reload the task list; no project metadata was changed.".into(),
                )
            })?;
        let spaces = self.workspace.read_spaces().map_err(|error| {
            (
                CommandReasonCode::OperationFailed,
                error.to_string(),
                "Reload the project catalog and retry.".into(),
            )
        })?;
        let destination = spaces
            .iter()
            .find(|space| space.id == destination_space_id)
            .cloned()
            .ok_or_else(|| {
                (
                    CommandReasonCode::InvalidDestination,
                    "the destination project is unavailable".into(),
                    "Refresh the authoritative project catalog; the original task is unchanged."
                        .into(),
                )
            })?;

        if destination.device_id != task.device_id
            || destination.device_id != self.engine_info.device_id
        {
            return Err((
                CommandReasonCode::IncompatibleEnvironment,
                "moving to another machine or execution environment requires X03 handoff".into(),
                "Choose a project on the task's owning machine, or wait for X03.".into(),
            ));
        }
        let cwd = task.cwd.as_deref().unwrap_or("~");
        let cwd = if cwd == "~" {
            home_dir()
        } else {
            std::path::PathBuf::from(cwd)
        };
        let destination_path = std::path::PathBuf::from(&destination.path);
        let canonical_cwd = std::fs::canonicalize(&cwd).map_err(|error| {
            (
                CommandReasonCode::InvalidDestination,
                format!("the task workspace cannot be validated: {error}"),
                "Restore access to the current workspace; the original task is unchanged.".into(),
            )
        })?;
        let canonical_destination = std::fs::canonicalize(&destination_path).map_err(|error| {
            (
                CommandReasonCode::InvalidDestination,
                format!("the destination folder cannot be validated: {error}"),
                "Choose an existing accessible folder; the original task is unchanged.".into(),
            )
        })?;
        let source_space = task
            .space_id
            .as_deref()
            .and_then(|space_id| spaces.iter().find(|space| space.id == space_id));
        let source_root = source_space
            .map(|space| std::path::PathBuf::from(&space.path))
            .unwrap_or_else(|| canonical_cwd.clone());
        let canonical_source = std::fs::canonicalize(&source_root).map_err(|error| {
            (
                CommandReasonCode::InvalidDestination,
                format!("the source workspace cannot be validated: {error}"),
                "Restore access to the current workspace; the original task is unchanged.".into(),
            )
        })?;
        if !canonical_cwd.starts_with(&canonical_source) {
            return Err((
                CommandReasonCode::InvalidDestination,
                "the task cwd no longer belongs to its source project".into(),
                "Refresh the source project catalog before retrying the move.".into(),
            ));
        }
        if !canonical_cwd.starts_with(&canonical_destination) {
            return Err((
                CommandReasonCode::IncompatibleWorkspace,
                "the destination project does not contain the task's current working directory; cross-workspace moves require X03"
                    .into(),
                "Choose a project that owns the same canonical workspace, or wait for X03."
                    .into(),
            ));
        }
        let source_is_git = self.repos.is_repo(&canonical_source).await;
        let destination_is_git = self.repos.is_repo(&canonical_destination).await;
        match (source_is_git, destination_is_git) {
            (true, true) => {
                let current_checkout =
                    self.repos
                        .checkout_identity(&canonical_cwd)
                        .await
                        .map_err(|error| {
                            (
                                CommandReasonCode::IncompatibleWorkspace,
                                format!("the current checkout cannot be validated: {error}"),
                                "Restore the current checkout before retrying the move.".into(),
                            )
                        })?;
                let destination_checkout = self
                    .repos
                    .checkout_identity(&canonical_destination)
                    .await
                    .map_err(|error| {
                        (
                            CommandReasonCode::IncompatibleWorkspace,
                            format!("the destination checkout cannot be validated: {error}"),
                            "Restore the destination checkout before retrying the move.".into(),
                        )
                    })?;
                if current_checkout.id != destination_checkout.id {
                    return Err((
                        CommandReasonCode::IncompatibleWorkspace,
                        "the destination resolves to a different checkout or worktree; cross-workspace moves require X03"
                            .into(),
                        "Choose a project in the same canonical checkout; the original task is unchanged."
                            .into(),
                    ));
                }
                if destination
                    .checkout_id
                    .as_deref()
                    .is_some_and(|id| id != destination_checkout.id)
                {
                    return Err((
                        CommandReasonCode::InvalidDestination,
                        "the destination project catalog is stale for its current checkout".into(),
                        "Wait for the owning machine to refresh the project catalog and retry."
                            .into(),
                    ));
                }
                if source_space
                    .and_then(|space| space.checkout_id.as_deref())
                    .is_some_and(|id| id != current_checkout.id)
                {
                    return Err((
                        CommandReasonCode::InvalidDestination,
                        "the source project catalog is stale for its current checkout".into(),
                        "Wait for the owning machine to refresh the project catalog and retry."
                            .into(),
                    ));
                }
            }
            (false, false) if canonical_source == canonical_destination => {}
            (false, false) => {
                return Err((
                    CommandReasonCode::IncompatibleWorkspace,
                    "non-Git project reassignment requires the exact same canonical workspace root"
                        .into(),
                    "Choose the same physical workspace root; different roots require X03.".into(),
                ));
            }
            _ => {
                return Err((
                    CommandReasonCode::IncompatibleWorkspace,
                    "the source and destination do not share one compatible workspace type".into(),
                    "Choose a destination in the same canonical workspace; the original task is unchanged."
                        .into(),
                ));
            }
        }
        Ok(ValidatedProjectMove { destination })
    }

    fn ensure_history_operation_ready(
        &self,
        task_id: &str,
    ) -> Result<(Chat, zeron_proto::ExecutionBinding), crate::EngineError> {
        let chat = self
            .workspace
            .chat(task_id)?
            .ok_or_else(|| crate::EngineError::Other("task no longer exists".into()))?;
        if chat.archived {
            return Err(crate::EngineError::Other(
                "archived tasks cannot fork or rewrite provider history".into(),
            ));
        }
        if self.sessions.active_turn_id(task_id).is_some() {
            return Err(crate::EngineError::Other(
                "wait for the active provider turn to settle before changing history".into(),
            ));
        }
        let handle = self.doc_host.open(task_id)?;
        if !handle.doc().read_queue()?.is_empty() {
            return Err(crate::EngineError::Other(
                "send or remove queued messages before changing history".into(),
            ));
        }
        if handle
            .doc()
            .read_commands()?
            .iter()
            .any(|command| command.status == SessionCommandStatus::Pending)
        {
            return Err(crate::EngineError::Other(
                "wait for pending task commands to settle before changing history".into(),
            ));
        }
        let unresolved = handle.doc().read_entries()?.iter().any(|entry| {
            entry.parts.iter().any(|part| {
                matches!(
                    part,
                    MessagePart::Input {
                        resolved: false,
                        ..
                    }
                )
            })
        });
        if unresolved {
            return Err(crate::EngineError::Other(
                "resolve the pending input or approval before changing history".into(),
            ));
        }
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding)
            .ok_or_else(|| {
                crate::EngineError::Other(
                    "provider-backed history requires a materialized execution binding".into(),
                )
            })?;
        if binding.provider_task_id.is_empty() {
            return Err(crate::EngineError::Other(
                "the provider binding has not materialized yet".into(),
            ));
        }
        let config = chat.config.as_ref().ok_or_else(|| {
            crate::EngineError::Other("task has no provider configuration".into())
        })?;
        if config.harness != binding.provider_id {
            return Err(crate::EngineError::Other(
                "task configuration and provider binding disagree".into(),
            ));
        }
        if capability_for(binding.provider_id) == zeron_proto::ProviderForkCapability::None {
            return Err(crate::EngineError::Other(
                unavailable_reason(binding.provider_id)
                    .unwrap_or("provider-backed fork is unsupported")
                    .into(),
            ));
        }
        Ok((chat, binding))
    }

    fn validate_history_review(
        &self,
        task_id: &str,
        chat: &Chat,
        binding: &zeron_proto::ExecutionBinding,
        review: &HistoryReviewToken,
    ) -> Result<(), crate::EngineError> {
        let snapshot = self
            .current_task_command_snapshot(task_id)
            .map_err(|error| crate::EngineError::Other(error.to_string()))?;
        if snapshot.task_revision != review.task_revision
            || binding.generation != review.binding_generation
            || binding.provider_task_id != review.provider_session_id
            || chat.cwd.as_deref().unwrap_or(".") != review.cwd
            || chat
                .config
                .as_ref()
                .and_then(|config| config.provider_account_id.as_ref())
                != review.provider_account_id.as_ref()
        {
            return Err(crate::EngineError::Other(
                "history confirmation is stale; reopen it from the current task".into(),
            ));
        }
        if let Some(message_id) = review.message_id.as_deref() {
            let handle = self.doc_host.open(task_id)?;
            let entry = handle
                .doc()
                .read_entries()?
                .into_iter()
                .find(|entry| entry.id == message_id)
                .ok_or_else(|| {
                    crate::EngineError::Other(
                        "the reviewed history message no longer exists".into(),
                    )
                })?;
            let text = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.trim()),
                    _ => None,
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if review.message_text.as_deref() != Some(text.as_str()) {
                return Err(crate::EngineError::Other(
                    "the reviewed history message changed; reopen the confirmation".into(),
                ));
            }
        }
        Ok(())
    }

    async fn fork_provider_history(
        &self,
        command_id: &str,
        chat: &Chat,
        binding: &zeron_proto::ExecutionBinding,
        boundary: zeron_proto::ProviderForkBoundary,
        require_current_binding: bool,
    ) -> Result<zeron_proto::ProviderForkResult, TaskProjectionError> {
        let config = chat.config.as_ref().ok_or_else(|| {
            TaskProjectionError::Mutation(crate::EngineError::Other(
                "task has no provider configuration".into(),
            ))
        })?;
        let cwd = chat.cwd.clone().unwrap_or_else(|| ".".into());
        if require_current_binding {
            self.task_identity
                .validate_explicit_resume(
                    &chat.id,
                    binding.provider_id,
                    &cwd,
                    &binding.provider_task_id,
                )
                .map_err(TaskProjectionError::Mutation)?;
        }
        let (provider_home, force_file_auth) = if binding.provider_id == HarnessId::Codex {
            let execution = self
                .agent_accounts
                .resolve_execution(HarnessId::Codex, config.provider_account_id.as_deref())
                .map_err(TaskProjectionError::Mutation)?;
            (
                execution
                    .codex_home
                    .map(|path| path.to_string_lossy().into_owned()),
                execution.codex_file_auth,
            )
        } else {
            (None, false)
        };
        let harness = self
            .registry
            .resolve(binding.provider_id)
            .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
        if harness.fork_capability() != capability_for(binding.provider_id) {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "provider fork capability changed; refresh before retrying".into(),
            )));
        }
        let progress_state = std::sync::Arc::new(std::sync::Mutex::new((false, None)));
        let tracked_progress = progress_state.clone();
        let doc_host = self.doc_host.clone();
        let tracked_command_id = command_id.to_string();
        let progress = move |event| match event {
            zeron_harness::ProviderForkProgress::MutationStarted => {
                tracked_progress.lock().unwrap().0 = true;
                Ok(())
            }
            zeron_harness::ProviderForkProgress::Acknowledged {
                provider_session_id,
            } => {
                {
                    let mut state = tracked_progress.lock().unwrap();
                    state.0 = true;
                    state.1 = Some(provider_session_id.clone());
                }
                match doc_host
                    .set_task_command_correlation(&tracked_command_id, &provider_session_id)
                {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(zeron_harness::HarnessError::Protocol(
                        "history command receipt vanished before provider acknowledgement".into(),
                    )),
                    Err(error) => Err(zeron_harness::HarnessError::Protocol(error.to_string())),
                }
            }
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            harness.fork_session_staged(
                ProviderForkRequest {
                    source_session_id: binding.provider_task_id.clone(),
                    boundary,
                    cwd,
                    model: config.model.clone(),
                    provider_home,
                    force_file_auth,
                },
                &progress,
            ),
        )
        .await;
        let mutation_started = progress_state.lock().unwrap().0;
        let result = match outcome {
            Ok(Ok(result)) => result,
            Ok(Err(error)) if mutation_started => {
                return Err(TaskProjectionError::ProviderUnknown(error.into()));
            }
            Ok(Err(error)) => return Err(TaskProjectionError::Mutation(error.into())),
            Err(_) if mutation_started => {
                return Err(TaskProjectionError::ProviderUnknown(
                    crate::EngineError::Other("provider fork completion is unknown".into()),
                ));
            }
            Err(_) => {
                return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                    "provider fork preflight timed out before mutation".into(),
                )));
            }
        };
        if result.provider_session_id == binding.provider_task_id {
            return Err(TaskProjectionError::ProviderUnknown(
                crate::EngineError::Other(
                    "provider fork did not return an independent session".into(),
                ),
            ));
        }
        if progress_state.lock().unwrap().1.as_deref() != Some(result.provider_session_id.as_str())
        {
            return Err(TaskProjectionError::ProviderUnknown(
                crate::EngineError::Other(
                    "provider fork returned without acknowledging its session id".into(),
                ),
            ));
        }
        Ok(result)
    }

    fn history_marker(&self, id: String, text: String) -> SessionMessageEntry {
        SessionMessageEntry {
            id,
            role: MessageRole::System,
            parts: vec![MessagePart::Text {
                id: "history-boundary".into(),
                text,
            }],
            created_at: crate::now_ms(),
            device_id: self.doc_host.device_id().to_string(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        }
    }

    fn inherited_run_request(
        &self,
        source_task_id: &str,
        prompt: &str,
        config: &ChatConfig,
        provider_session_id: &str,
    ) -> Result<zeron_proto::RunRequest, crate::EngineError> {
        let mut request = self
            .doc_host
            .request_from_chat_row(source_task_id, prompt)
            .ok_or_else(|| {
                crate::EngineError::Other("task execution settings are unavailable".into())
            })?;
        request.prompt = prompt.to_string();
        request.harness = Some(config.harness);
        request.provider_account_id = config.provider_account_id.clone();
        request.model = config.model.clone();
        request.reasoning = config.reasoning;
        request.model_options = config.model_options.clone();
        request.sandbox = config.sandbox;
        request.permission_mode = config.permission_mode;
        request.resume = Some(provider_session_id.to_string());
        request.attachments.clear();
        request.worktree = None;
        Ok(request)
    }

    fn queue_initial_history_prompt(
        &self,
        doc: &SessionDoc,
        command_id: &str,
        message_id: &str,
        request: zeron_proto::RunRequest,
    ) -> Result<(), crate::EngineError> {
        doc.queue_command(&SessionCommandEntry {
            id: command_id.to_string(),
            payload: SessionCommandPayload::Run {
                request,
                message_id: message_id.to_string(),
            },
            issued_by: self.doc_host.device_id().to_string(),
            issued_at: crate::now_ms(),
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
        })?;
        Ok(())
    }

    fn commit_task_projection<R>(
        &self,
        command_id: &str,
        task_id: &str,
        mutation: impl FnOnce(&mut RegistryDoc) -> Result<R, crate::EngineError>,
    ) -> Result<(R, CommandReceipt), TaskProjectionError> {
        let current = self
            .current_task_command_snapshot(task_id)
            .map_err(|error| {
                TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
            })?;
        let committed = self.workspace.commit_task_command_projection(
            command_id,
            current.binding_id.as_deref(),
            current.binding_generation,
            current.active_turn_id.as_deref(),
            mutation,
        )?;
        Ok(committed)
    }

    fn commit_task_projection_superseding_recovery<R>(
        &self,
        command_id: &str,
        task_id: &str,
        recovery_detail: &str,
        mutation: impl FnOnce(&mut RegistryDoc) -> Result<R, crate::EngineError>,
    ) -> Result<(R, CommandReceipt), TaskProjectionError> {
        let current = self
            .current_task_command_snapshot(task_id)
            .map_err(|error| {
                TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
            })?;
        let recovery = self
            .sessions
            .prepare_supersede_recovery(task_id, recovery_detail)
            .map_err(TaskProjectionError::Mutation)?;
        let committed = if let Some(change) = recovery.as_ref() {
            let transition = change
                .storage_transition()
                .map_err(TaskProjectionError::Mutation)?;
            self.workspace.commit_task_command_projection_with_recovery(
                command_id,
                current.binding_id.as_deref(),
                current.binding_generation,
                current.active_turn_id.as_deref(),
                &transition,
                mutation,
            )
        } else {
            self.workspace.commit_task_command_projection(
                command_id,
                current.binding_id.as_deref(),
                current.binding_generation,
                current.active_turn_id.as_deref(),
                mutation,
            )
        }?;
        if let Some(change) = recovery.as_ref() {
            self.sessions.publish_committed_recovery(&change.next);
        }
        Ok(committed)
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_task_projection_with_changes<R>(
        &self,
        command_id: &str,
        current: &TaskCommandSnapshot,
        identity_snapshot: Option<(&str, &[u8])>,
        deleted_chat_snapshot: Option<&str>,
        mutation: impl FnOnce(&mut RegistryDoc) -> Result<R, crate::EngineError>,
    ) -> Result<(R, CommandReceipt), TaskProjectionError> {
        self.workspace.commit_task_command_projection_with_changes(
            command_id,
            current.binding_id.as_deref(),
            current.binding_generation,
            current.active_turn_id.as_deref(),
            identity_snapshot,
            deleted_chat_snapshot,
            mutation,
        )
    }

    fn reject_context_command(
        &self,
        command_id: &str,
        code: CommandReasonCode,
        message: &str,
        recovery: &str,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        self.finish_task_command(
            command_id,
            CommandReceiptStatus::Rejected,
            false,
            code,
            message,
            recovery,
        )
        .map(Some)
        .map_err(|error| TaskProjectionError::Commit(crate::EngineError::Other(error.to_string())))
    }

    async fn apply_compact_context(
        &self,
        command_id: &str,
        task_id: &str,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(TaskProjectionError::Mutation)?
            .ok_or_else(|| {
                TaskProjectionError::Mutation(crate::EngineError::Other(
                    "context compaction requires an existing task".into(),
                ))
            })?;
        let config = chat.config.ok_or_else(|| {
            TaskProjectionError::Mutation(crate::EngineError::Other(
                "context compaction requires a provider configuration".into(),
            ))
        })?;
        let capabilities = crate::context_controls::capabilities_for(config.harness);
        let Some(mechanism) = capabilities.compact else {
            return self.reject_context_command(
                command_id,
                CommandReasonCode::UnsupportedOperation,
                capabilities
                    .compact_unavailable_reason
                    .as_deref()
                    .unwrap_or("Context compaction is unsupported."),
                "Continue normally or switch to a harness with a verified native compact operation.",
            );
        };
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding)
            .ok_or_else(|| {
                TaskProjectionError::Mutation(crate::EngineError::Other(
                    "context compaction requires an accepted provider binding".into(),
                ))
            })?;
        if binding.provider_id != config.harness || binding.provider_task_id.is_empty() {
            return self.reject_context_command(
                command_id,
                CommandReasonCode::StaleBindingGeneration,
                "The task has no current provider session to compact.",
                "Send the first prompt on the fresh binding before compacting it.",
            );
        }
        let mut context = self
            .doc_host
            .context_control(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        if context.compact.blocks_new_turn() {
            if context.compact.status == ContextOperationStatus::Running {
                context.compact.status = ContextOperationStatus::Unknown;
                context.compact.finished_at_ms = Some(crate::now_ms());
                context.compact.detail = Some(
                    "A prior compaction did not produce a durable completion readback. It will not be replayed."
                        .into(),
                );
                self.doc_host
                    .persist_context_control(task_id, command_id, &context)
                    .map_err(TaskProjectionError::Mutation)?;
            }
            return self.reject_context_command(
                command_id,
                CommandReasonCode::ProviderAcceptanceUnknown,
                context
                    .compact
                    .detail
                    .as_deref()
                    .unwrap_or("A prior compaction is unresolved."),
                "Do not retry blindly. Clear context to fence the old operation, or wait for provider readback.",
            );
        }
        context.binding_generation = Some(binding.generation);
        context.compact = CompactOperation {
            operation_id: Some(command_id.to_string()),
            status: ContextOperationStatus::Running,
            mechanism: Some(mechanism),
            binding_generation: Some(binding.generation),
            started_at_ms: Some(crate::now_ms()),
            finished_at_ms: None,
            detail: Some("Provider-owned context compaction is running.".into()),
        };
        context.notice = None;
        self.doc_host
            .persist_context_control(task_id, command_id, &context)
            .map_err(TaskProjectionError::Mutation)?;

        let cwd = chat.cwd.unwrap_or_else(|| ".".into());
        let result = match config.harness {
            HarnessId::Codex => {
                let execution = self
                    .agent_accounts
                    .resolve_execution(HarnessId::Codex, config.provider_account_id.as_deref());
                match execution {
                    Ok(execution) => match execution.codex_home {
                        Some(codex_home) => {
                            self.task_identity
                                .compact_task(
                                    task_id,
                                    &codex_home,
                                    execution.codex_file_auth,
                                    &cwd,
                                    config.model.as_deref(),
                                )
                                .await
                        }
                        None => Err(crate::EngineError::Other(
                            "Codex account did not resolve an execution home".into(),
                        )),
                    },
                    Err(error) => Err(error),
                }
            }
            harness => tokio::time::timeout(
                Duration::from_secs(30),
                self.sessions
                    .compact_context(harness, binding.provider_task_id, cwd, config.model),
            )
            .await
            .map_err(|_| crate::EngineError::Other("Provider compaction timed out".into()))
            .and_then(|result| result),
        };
        let now = crate::now_ms();
        match result {
            Ok(()) => {
                context.compact.status = ContextOperationStatus::Completed;
                context.compact.finished_at_ms = Some(now);
                context.compact.detail =
                    Some("The provider reported that context compaction completed.".into());
                let receipt = self
                    .doc_host
                    .complete_context_operation(
                        task_id,
                        command_id,
                        CommandReceiptStatus::Applied,
                        true,
                        None,
                        None,
                        &context,
                        true,
                    )
                    .map_err(TaskProjectionError::Commit)?;
                Ok(Some(receipt))
            }
            Err(error) => {
                context.compact.status = ContextOperationStatus::Unknown;
                context.compact.finished_at_ms = Some(now);
                context.compact.detail = Some(format!(
                    "Compaction completion is unknown: {error}. LUNA will not replay it."
                ));
                let receipt = self
                    .doc_host
                    .complete_context_operation(
                        task_id,
                        command_id,
                        CommandReceiptStatus::Failed,
                        false,
                        Some(CommandReceiptReason {
                            code: CommandReasonCode::ProviderAcceptanceUnknown,
                            message: context.compact.detail.clone().unwrap_or_default(),
                        }),
                        Some(
                            "Clear context to create a fresh binding, or wait for provider readback; do not retry compaction blindly.",
                        ),
                        &context,
                        false,
                    )
                    .map_err(TaskProjectionError::Commit)?;
                Ok(Some(receipt))
            }
        }
    }

    fn visible_handoff(
        &self,
        task_id: &str,
        requested: &ContextHandoff,
    ) -> Result<ContextHandoff, crate::EngineError> {
        if requested.message_ids.is_empty()
            || requested.message_ids.len() > zeron_proto::MAX_CONTEXT_HANDOFF_MESSAGES
        {
            return Err(crate::EngineError::Other(format!(
                "context handoff must contain 1–{} visible messages and at most {} bytes",
                zeron_proto::MAX_CONTEXT_HANDOFF_MESSAGES,
                zeron_proto::MAX_CONTEXT_HANDOFF_BYTES
            )));
        }
        let entries = self.doc_host.open(task_id)?.doc().read_entries()?;
        let mut seen = std::collections::HashSet::new();
        let mut resolved = Vec::new();
        for message_id in &requested.message_ids {
            if !seen.insert(message_id.as_str()) {
                return Err(crate::EngineError::Other(
                    "context handoff message ids must be unique".into(),
                ));
            }
            let entry = entries
                .iter()
                .find(|entry| entry.id == *message_id)
                .ok_or_else(|| {
                    crate::EngineError::Other(format!(
                        "context handoff message {message_id} is not in this task"
                    ))
                })?;
            let role = match entry.role {
                zeron_doc::MessageRole::User => "User",
                zeron_doc::MessageRole::Assistant => "Assistant",
                zeron_doc::MessageRole::System => {
                    return Err(crate::EngineError::Other(
                        "system messages cannot be included in a provider handoff".into(),
                    ));
                }
            };
            let visible = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    zeron_doc::MessagePart::Text { text, .. } => Some(text.trim()),
                    _ => None,
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if visible.is_empty() {
                return Err(crate::EngineError::Other(format!(
                    "context handoff message {message_id} has no visible conversation text"
                )));
            }
            resolved.push((message_id.clone(), role, visible));
        }
        // Newest messages have priority when the bounded handoff cannot fit
        // every selected entry; reverse again before serializing so the
        // provider still receives chronological conversation order.
        let mut selected = Vec::new();
        let mut bytes = 0usize;
        for (message_id, role, visible) in resolved.into_iter().rev() {
            let separator = usize::from(!selected.is_empty()) * 2;
            let prefix = format!("{role}: ");
            let remaining = zeron_proto::MAX_CONTEXT_HANDOFF_BYTES.saturating_sub(bytes);
            if remaining <= separator + prefix.len() {
                break;
            }
            let mut end = visible
                .len()
                .min(remaining.saturating_sub(separator + prefix.len()));
            while !visible.is_char_boundary(end) {
                end -= 1;
            }
            if end == 0 {
                break;
            }
            let block = format!("{prefix}{}", &visible[..end]);
            bytes += separator + block.len();
            selected.push((message_id, block));
        }
        if selected.is_empty() {
            return Err(crate::EngineError::Other(
                "context handoff has no visible conversation text".into(),
            ));
        }
        selected.reverse();
        Ok(ContextHandoff {
            kind: requested.kind,
            text: selected
                .iter()
                .map(|(_, block)| block.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            message_ids: selected.into_iter().map(|(id, _)| id).collect(),
        })
    }

    fn apply_context_boundary(
        &self,
        command_id: &str,
        task_id: &str,
        target_config: ChatConfig,
        kind: ContextBoundaryKind,
        handoff: Option<ContextHandoff>,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        let handoff = handoff
            .as_ref()
            .map(|handoff| self.visible_handoff(task_id, handoff))
            .transpose()
            .map_err(TaskProjectionError::Mutation)?;
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(TaskProjectionError::Mutation)?
            .ok_or_else(|| {
                TaskProjectionError::Mutation(crate::EngineError::Other(
                    "context boundary requires an existing task".into(),
                ))
            })?;
        let current_config = chat.config.ok_or_else(|| {
            TaskProjectionError::Mutation(crate::EngineError::Other(
                "context boundary requires a provider configuration".into(),
            ))
        })?;
        if kind == ContextBoundaryKind::Cleared && target_config.harness != current_config.harness {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "clear context cannot change providers".into(),
            )));
        }
        if kind == ContextBoundaryKind::ProviderSwitched
            && target_config.harness == current_config.harness
        {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "provider switch requires a different harness".into(),
            )));
        }
        let cwd = chat.cwd.unwrap_or_else(|| ".".into());
        let recovery = self
            .sessions
            .prepare_supersede_recovery(
                task_id,
                match kind {
                    ContextBoundaryKind::Cleared => {
                        "Recovery cancelled because context was explicitly cleared."
                    }
                    ContextBoundaryKind::ProviderSwitched => {
                        "Recovery cancelled because the provider binding changed."
                    }
                },
            )
            .map_err(TaskProjectionError::Mutation)?;
        let boundary_id = format!("context-boundary:{command_id}");
        let origin = match kind {
            ContextBoundaryKind::Cleared => BindingOrigin::ContextReset,
            ContextBoundaryKind::ProviderSwitched => BindingOrigin::ProviderSwitch,
        };
        let provenance = match kind {
            ContextBoundaryKind::Cleared => "user explicitly cleared provider context",
            ContextBoundaryKind::ProviderSwitched => {
                "user explicitly switched providers with a fresh execution binding"
            }
        };
        let transition = recovery
            .as_ref()
            .map(|change| change.storage_transition())
            .transpose()
            .map_err(TaskProjectionError::Mutation)?;
        let target_provider = target_config.harness;
        let handoff_for_state = handoff.clone();
        let target_for_registry = target_config.clone();
        let cwd_for_registry = cwd.clone();
        let committed = self.task_identity.begin_fresh_binding_with_command(
            task_id,
            target_provider,
            origin,
            provenance,
            |binding, identity_snapshot| {
                let boundary = ContextBoundary {
                    boundary_id: boundary_id.clone(),
                    kind,
                    generation: binding.generation,
                    from_provider: current_config.harness,
                    to_provider: target_provider,
                    created_at_ms: crate::now_ms(),
                    handoff_kind: handoff_for_state.as_ref().map(|handoff| handoff.kind),
                };
                let state = ContextControlState {
                    binding_generation: Some(binding.generation),
                    compact: CompactOperation::default(),
                    last_boundary: Some(boundary),
                    pending_handoff: handoff_for_state.clone(),
                    last_handoff: handoff_for_state.clone(),
                    provider_summary: None,
                    notice: Some(match kind {
                        ContextBoundaryKind::Cleared => {
                            "Old provider context was cleared. The next prompt starts a fresh provider session; LUNA history and files remain."
                                .into()
                        }
                        ContextBoundaryKind::ProviderSwitched if handoff_for_state.is_some() => {
                            "A fresh provider binding will receive only the selected visible-history handoff."
                                .into()
                        }
                        ContextBoundaryKind::ProviderSwitched => {
                            "A fresh provider binding will start without prior model context.".into()
                        }
                    }),
                    ..Default::default()
                };
                let boundary_text = state.notice.clone().unwrap_or_default();
                self.doc_host.commit_context_boundary(
                    task_id,
                    command_id,
                    Some(&binding.binding_id),
                    Some(binding.generation),
                    identity_snapshot,
                    transition.as_ref(),
                    &[],
                    move |doc| {
                        doc.clear_context_usage()?;
                        doc.set_context_control(&state)?;
                        if !doc
                            .read_entries()?
                            .iter()
                            .any(|entry| entry.id == boundary_id)
                        {
                            doc.push_message(&zeron_doc::SessionMessageEntry {
                                id: boundary_id,
                                role: zeron_doc::MessageRole::System,
                                parts: vec![zeron_doc::MessagePart::Text {
                                    id: "boundary".into(),
                                    text: boundary_text,
                                }],
                                created_at: crate::now_ms(),
                                device_id: self.doc_host.device_id().to_string(),
                                status: Some(zeron_doc::MessageStatus::Complete),
                                continuation_of: None,
                            })?;
                        }
                        let held = doc.hold_queue_for_context_change(
                            target_provider,
                            kind == ContextBoundaryKind::Cleared,
                            crate::now_ms(),
                            |text| format!("{:x}", Sha256::digest(text.as_bytes())),
                        )?;
                        Ok(held)
                    },
                    move |registry| {
                        if !registry.set_chat_config(task_id, &target_for_registry)? {
                            return Err(crate::EngineError::Other(
                                "task vanished during context boundary".into(),
                            ));
                        }
                        registry.set_chat_harness_session(task_id, "", &cwd_for_registry)?;
                        Ok(())
                    },
                )
            },
        )?;
        if let Some(change) = recovery.as_ref() {
            self.sessions.publish_committed_recovery(&change.next);
        }
        self.sessions.retire_context_binding(task_id, &cwd);
        Ok(Some(committed.1))
    }

    fn apply_model_change(
        &self,
        command_id: &str,
        task_id: &str,
        requested: ChatConfig,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(TaskProjectionError::Mutation)?
            .ok_or_else(|| {
                TaskProjectionError::Mutation(crate::EngineError::Other(
                    "model change requires an existing task".into(),
                ))
            })?;
        let current = chat.config.ok_or_else(|| {
            TaskProjectionError::Mutation(crate::EngineError::Other(
                "model change requires a provider configuration".into(),
            ))
        })?;
        if current.harness != requested.harness {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "use the explicit provider-switch action to change harnesses".into(),
            )));
        }
        let mut config = current.clone();
        config.model = requested.model;
        config.reasoning = requested.reasoning;
        config.model_options = requested.model_options;
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding);
        if binding
            .as_ref()
            .is_some_and(|binding| binding.provider_id != current.harness)
        {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "model change does not match the current provider binding".into(),
            )));
        }
        let recovery = self
            .sessions
            .prepare_supersede_recovery(
                task_id,
                "Recovery cancelled because the next-turn model changed.",
            )
            .map_err(TaskProjectionError::Mutation)?;
        let transition = recovery
            .as_ref()
            .map(|change| change.storage_transition())
            .transpose()
            .map_err(TaskProjectionError::Mutation)?;
        let mut state = self
            .doc_host
            .context_control(task_id)
            .map_err(TaskProjectionError::Mutation)?;
        state.binding_generation = binding.as_ref().map(|binding| binding.generation);
        state.notice =
            Some(crate::context_controls::capabilities_for(config.harness).model_change_note);
        let state_for_doc = state;
        let config_for_registry = config;
        let binding_id = binding.as_ref().map(|binding| binding.binding_id.as_str());
        let binding_generation = binding.as_ref().map(|binding| binding.generation);
        let (_, receipt) = self.doc_host.commit_context_boundary(
            task_id,
            command_id,
            binding_id,
            binding_generation,
            None,
            transition.as_ref(),
            &[],
            move |doc| {
                doc.set_context_control(&state_for_doc)?;
                Ok(())
            },
            move |registry| {
                if !registry.set_chat_config(task_id, &config_for_registry)? {
                    return Err(crate::EngineError::Other(
                        "task vanished during model change".into(),
                    ));
                }
                Ok(())
            },
        )?;
        if let Some(change) = recovery.as_ref() {
            self.sessions.publish_committed_recovery(&change.next);
        }
        Ok(Some(receipt))
    }

    async fn apply_task_command_intent(
        &self,
        command_id: &str,
        task_id: &str,
        intent: TaskCommandIntent,
        expected_turn_id: Option<&str>,
        validated_project_move: Option<ValidatedProjectMove>,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        match intent {
            TaskCommandIntent::CreateChat {
                space_id,
                device_id,
                config,
                branch,
                cwd,
                lineage,
            } => {
                let identity_space_id = space_id.clone();
                let identity_device_id = device_id.clone();
                let identity_cwd = cwd.clone();
                let owned_task_id = task_id.to_string();
                let current = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                let (_, receipt) = self.task_identity.register_new_task_with_command(
                    task_id,
                    identity_space_id.as_deref(),
                    identity_device_id.as_deref(),
                    identity_cwd.as_deref(),
                    lineage,
                    move |identity_snapshot| {
                        self.commit_task_projection_with_changes(
                            command_id,
                            &current,
                            identity_snapshot,
                            None,
                            move |doc| {
                                if doc.chat(&owned_task_id)?.is_some() {
                                    return Ok(());
                                }
                                let space = match space_id.as_deref() {
                                    Some(space_id) => {
                                        Some(doc.space(space_id)?.ok_or_else(|| {
                                            crate::EngineError::Other(format!(
                                                "no such space: {space_id}"
                                            ))
                                        })?)
                                    }
                                    None => None,
                                };
                                let host_device = match (&space, device_id.as_deref()) {
                                    (Some(space), _) => space.device_id.clone(),
                                    (None, Some(device_id)) => device_id.to_string(),
                                    (None, None) => {
                                        return Err(crate::EngineError::Other(
                                            "createChat needs a spaceId or a deviceId".into(),
                                        ));
                                    }
                                };
                                doc.upsert_chat(&Chat {
                                    id: owned_task_id.clone(),
                                    device_id: host_device,
                                    title: None,
                                    archived: false,
                                    pinned_order: None,
                                    cwd: Some(cwd.unwrap_or_else(|| {
                                        space
                                            .as_ref()
                                            .map(|space| space.path.clone())
                                            .unwrap_or_else(|| "~".into())
                                    })),
                                    branch: None,
                                    checkout_id: None,
                                    source_context: None,
                                    config,
                                    last_message_preview: None,
                                    last_message_at: None,
                                    created_at: chrono::Utc::now(),
                                    harness_session_id: None,
                                    room_gen: Some(2),
                                    harness_session_cwd: None,
                                    space_id: space.as_ref().map(|space| space.id.clone()),
                                    last_seen_at: None,
                                    manually_unread: false,
                                })?;
                                if let Some(branch) =
                                    branch.as_deref().filter(|branch| !branch.is_empty())
                                {
                                    doc.set_chat_branch(&owned_task_id, branch)?;
                                }
                                Ok(())
                            },
                        )
                    },
                )?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::ForkTask {
                new_task_id,
                boundary,
                message_id,
                title,
                initial_prompt,
                config,
                source_link,
                review,
            } => {
                if uuid::Uuid::parse_str(&new_task_id).is_err() {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "fork target must use a stable UUID task id".into(),
                    )));
                }
                if self.task_identity.view(&new_task_id).is_some()
                    || self
                        .workspace
                        .chat(&new_task_id)
                        .map_err(TaskProjectionError::Mutation)?
                        .is_some()
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "fork target task already exists".into(),
                    )));
                }
                let before = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                let (source, binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                self.validate_history_review(task_id, &source, &binding, &review)
                    .map_err(TaskProjectionError::Mutation)?;
                let handle = self
                    .doc_host
                    .open(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                let plan = plan_history(
                    handle.doc(),
                    binding.provider_id,
                    boundary,
                    message_id.as_deref(),
                )
                .map_err(TaskProjectionError::Mutation)?;
                let source_config = source.config.clone().ok_or_else(|| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "source task has no provider configuration".into(),
                    ))
                })?;
                let mut target_config = config.unwrap_or_else(|| source_config.clone());
                if target_config.harness != source_config.harness
                    || target_config.provider_account_id != source_config.provider_account_id
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "a native fork must keep the source provider and provider account".into(),
                    )));
                }
                let permission_ceiling = source_config.effective_permission_mode;
                target_config.permission_mode =
                    std::cmp::min(target_config.permission_mode, permission_ceiling);
                target_config.effective_permission_mode = target_config.permission_mode;
                target_config.permission_ceiling_source = source_config.permission_ceiling_source;
                target_config.permission_degradation_reason =
                    source_config.permission_degradation_reason;
                let source_link = source_link
                    .filter(|link| {
                        link.starts_with(&format!("luna://open/chat/{task_id}?workspace="))
                    })
                    .ok_or_else(|| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            "fork lineage link is missing or invalid".into(),
                        ))
                    })?;

                let provider_result = self
                    .fork_provider_history(
                        command_id,
                        &source,
                        &binding,
                        plan.provider_boundary.clone(),
                        true,
                    )
                    .await?;
                let provider_session_id = provider_result.provider_session_id.clone();
                let remapped_checkpoints = plan
                    .remapped_checkpoints(&provider_result)
                    .map_err(TaskProjectionError::ProviderUnknown)?;

                let after = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            error.to_string(),
                        ))
                    })?;
                let (current_source, current_binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                if before.task_revision != after.task_revision
                    || before.task_sequence != after.task_sequence
                    || binding.binding_id != current_binding.binding_id
                    || source.cwd != current_source.cwd
                    || source.space_id != current_source.space_id
                {
                    return Err(TaskProjectionError::ProviderUnknown(
                        crate::EngineError::Other(
                            "source task changed while the provider fork was being prepared".into(),
                        ),
                    ));
                }

                let target_doc = SessionDoc::init(&new_task_id)
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                target_doc
                    .replace_conversation_entries(&plan.entries, &remapped_checkpoints)
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                target_doc
                    .push_message(&self.history_marker(
                        format!("fork-source:{command_id}"),
                        format!(
                            "Forked from [{task_id}]({source_link}). This task has independent provider history and lifecycle ownership."
                        ),
                    ))
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                let prompt = initial_prompt
                    .map(|prompt| prompt.trim().to_string())
                    .filter(|prompt| !prompt.is_empty());
                if let Some(prompt) = prompt.as_deref() {
                    let request = self
                        .inherited_run_request(
                            task_id,
                            prompt,
                            &target_config,
                            &provider_session_id,
                        )
                        .map_err(TaskProjectionError::ProviderUnknown)?;
                    self.queue_initial_history_prompt(
                        &target_doc,
                        &format!("fork-run:{command_id}"),
                        &uuid::Uuid::new_v4().to_string(),
                        request,
                    )
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                }
                let target_snapshot = target_doc
                    .export_snapshot()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                let target_title = title
                    .map(|title| title.trim().to_string())
                    .filter(|title| !title.is_empty())
                    .or_else(|| source.title.as_ref().map(|title| format!("{title} (fork)")));
                let source_for_registry = source.clone();
                let new_task_for_registry = new_task_id.clone();
                let provider_for_identity = binding.provider_id;
                let provider_for_identity_id = provider_session_id.clone();
                let provider_for_registry_id = provider_for_identity_id.clone();
                let receipt = self
                    .task_identity
                    .register_fork_task_with_command(
                        task_id,
                        &new_task_id,
                        provider_for_identity,
                        &provider_for_identity_id,
                        |_, identity_snapshot| {
                            let (_, receipt) = self.workspace.commit_context_boundary(
                                command_id,
                                before.binding_id.as_deref(),
                                before.binding_generation,
                                identity_snapshot,
                                &[(new_task_id.as_str(), target_snapshot.as_slice())],
                                &[],
                                None,
                                move |registry| {
                                    registry.upsert_chat(&Chat {
                                        id: new_task_for_registry,
                                        device_id: source_for_registry.device_id,
                                        title: target_title,
                                        archived: false,
                                        pinned_order: None,
                                        cwd: source_for_registry.cwd.clone(),
                                        branch: source_for_registry.branch,
                                        checkout_id: source_for_registry.checkout_id,
                                        source_context: source_for_registry.source_context,
                                        config: Some(target_config),
                                        last_message_preview: Some(format!(
                                            "Forked from {}",
                                            source_for_registry
                                                .title
                                                .as_deref()
                                                .unwrap_or("source task")
                                        )),
                                        last_message_at: source_for_registry.last_message_at,
                                        created_at: chrono::Utc::now(),
                                        harness_session_id: Some(provider_for_registry_id.clone()),
                                        harness_session_cwd: source_for_registry.cwd,
                                        space_id: source_for_registry.space_id,
                                        last_seen_at: None,
                                        manually_unread: false,
                                        room_gen: source_for_registry.room_gen,
                                    })?;
                                    Ok(())
                                },
                            )?;
                            Ok(receipt)
                        },
                    )
                    .map_err(|error| {
                        TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            error.message().to_string(),
                        ))
                    })?;
                if prompt.is_some() {
                    let _ = self.doc_host.open(&new_task_id);
                }
                Ok(Some(receipt))
            }
            TaskCommandIntent::EditEarlierMessage {
                message_id,
                text,
                review,
            } => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "edited message cannot be empty".into(),
                    )));
                }
                let before = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                let (source, binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                self.validate_history_review(task_id, &source, &binding, &review)
                    .map_err(TaskProjectionError::Mutation)?;
                let handle = self
                    .doc_host
                    .open(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                let source_snapshot = handle
                    .doc()
                    .export_snapshot()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?;
                let plan = plan_history(
                    handle.doc(),
                    binding.provider_id,
                    ConversationForkBoundaryKind::BeforeUserMessage,
                    Some(&message_id),
                )
                .map_err(TaskProjectionError::Mutation)?;
                let source_tip_plan = plan_history(
                    handle.doc(),
                    binding.provider_id,
                    ConversationForkBoundaryKind::Tip,
                    None,
                )
                .map_err(TaskProjectionError::Mutation)?;
                let source_tip_checkpoint_id = source_tip_plan
                    .tip_checkpoint()
                    .ok_or_else(|| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            "the pre-edit conversation has no exact completed provider tip".into(),
                        ))
                    })?
                    .to_owned();
                if plan.selected_text.as_deref() == Some(text.as_str()) {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "edited message is unchanged".into(),
                    )));
                }
                let config = source.config.clone().ok_or_else(|| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "task has no provider configuration".into(),
                    ))
                })?;
                let provider_result = self
                    .fork_provider_history(
                        command_id,
                        &source,
                        &binding,
                        plan.provider_boundary.clone(),
                        true,
                    )
                    .await?;
                let provider_session_id = provider_result.provider_session_id.clone();
                let remapped_checkpoints = plan
                    .remapped_checkpoints(&provider_result)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                let after = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            error.to_string(),
                        ))
                    })?;
                let (_, current_binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                if before.task_revision != after.task_revision
                    || before.task_sequence != after.task_sequence
                    || binding.binding_id != current_binding.binding_id
                {
                    return Err(TaskProjectionError::ProviderUnknown(
                        crate::EngineError::Other(
                            "task changed while the provider rewind was being prepared".into(),
                        ),
                    ));
                }

                let recovery = prepare_recovery(
                    &self.store,
                    HistoryRecoveryView {
                        recovery_id: command_id.to_string(),
                        task_id: task_id.to_string(),
                        source_binding_id: binding.binding_id.clone(),
                        source_provider_session_id: binding.provider_task_id.clone(),
                        cutoff_message_id: message_id.clone(),
                        source_tip_checkpoint_id,
                        created_at_ms: crate::now_ms(),
                        summary: "Conversation history before an earlier-message edit. Files and external effects were not rewound."
                            .into(),
                    },
                )
                .map_err(TaskProjectionError::ProviderUnknown)?;
                let request = self
                    .inherited_run_request(task_id, &text, &config, &provider_session_id)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                let cwd = source.cwd.clone().unwrap_or_else(|| ".".into());
                self.sessions.retire_context_binding(task_id, &cwd);
                let plan_entries = plan.entries;
                let plan_checkpoints = remapped_checkpoints;
                let marker = self.history_marker(
                    format!("history-edit:{command_id}"),
                    format!(
                        "Conversation rewritten from this point. Files, tools, and external effects were not undone. Recovery id: {}.",
                        recovery.view.recovery_id
                    ),
                );
                let run_command_id = format!("history-edit-run:{command_id}");
                let provider_for_registry = provider_session_id.clone();
                let task_for_registry = task_id.to_string();
                let receipt = self
                    .task_identity
                    .replace_with_forked_binding_with_command(
                        task_id,
                        &provider_session_id,
                        |new_binding, identity_snapshot| {
                            let extra = [
                                (recovery.index_id.as_str(), recovery.index_bytes.as_slice()),
                                (recovery.snapshot_id.as_str(), source_snapshot.as_slice()),
                            ];
                            let (_, receipt) = self.doc_host.commit_context_boundary(
                                task_id,
                                command_id,
                                Some(&new_binding.binding_id),
                                Some(new_binding.generation),
                                identity_snapshot,
                                None,
                                &extra,
                                move |doc| {
                                    doc.replace_conversation_entries(
                                        &plan_entries,
                                        &plan_checkpoints,
                                    )?;
                                    doc.push_message(&marker)?;
                                    let mut context = ContextControlState::default();
                                    context.binding_generation = Some(new_binding.generation);
                                    context.notice = Some(
                                        "Context controls reset for the forked provider binding."
                                            .into(),
                                    );
                                    doc.set_context_control(&context)?;
                                    self.queue_initial_history_prompt(
                                        doc,
                                        &run_command_id,
                                        &message_id,
                                        request,
                                    )?;
                                    Ok(())
                                },
                                move |registry| {
                                    if !registry.set_chat_harness_session(
                                        &task_for_registry,
                                        &provider_for_registry,
                                        &cwd,
                                    )? {
                                        return Err(crate::EngineError::Other(
                                            "task vanished during history rewrite".into(),
                                        ));
                                    }
                                    Ok(())
                                },
                            )?;
                            Ok(receipt)
                        },
                    )
                    .map_err(|error| {
                        TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            error.message().to_string(),
                        ))
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::RestoreHistoryRecovery {
                recovery_id,
                review,
            } => {
                let before = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                let (source, binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                self.validate_history_review(task_id, &source, &binding, &review)
                    .map_err(TaskProjectionError::Mutation)?;
                let (recovery_view, recovery_snapshot) =
                    load_recovery(&self.store, task_id, &recovery_id)
                        .map_err(TaskProjectionError::Mutation)?;
                if review.message_id.is_some()
                    || review.message_text.as_deref()
                        != Some(recovery_view.source_provider_session_id.as_str())
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "history recovery selection is stale; reopen the recovery dialog".into(),
                    )));
                }
                let old_binding = self
                    .task_identity
                    .view(task_id)
                    .and_then(|view| {
                        view.bindings.into_iter().find(|candidate| {
                            candidate.binding_id == recovery_view.source_binding_id
                        })
                    })
                    .ok_or_else(|| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            "history recovery source binding is unavailable".into(),
                        ))
                    })?;
                if old_binding.provider_id != binding.provider_id
                    || old_binding.provider_task_id != recovery_view.source_provider_session_id
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "history recovery no longer matches its provider source".into(),
                    )));
                }
                let recovered_doc = SessionDoc::from_snapshot(&recovery_snapshot)
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?;
                let plan = plan_history(
                    &recovered_doc,
                    binding.provider_id,
                    ConversationForkBoundaryKind::Tip,
                    None,
                )
                .map_err(TaskProjectionError::Mutation)?;
                if recovery_view.source_tip_checkpoint_id.is_empty()
                    || plan.tip_checkpoint()
                        != Some(recovery_view.source_tip_checkpoint_id.as_str())
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "history recovery has no verified exact native tip checkpoint".into(),
                    )));
                }
                let current_handle = self
                    .doc_host
                    .open(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                let current_snapshot = current_handle
                    .doc()
                    .export_snapshot()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?;
                let current_plan = plan_history(
                    current_handle.doc(),
                    binding.provider_id,
                    ConversationForkBoundaryKind::Tip,
                    None,
                )
                .map_err(TaskProjectionError::Mutation)?;
                let current_tip_checkpoint_id = current_plan
                    .tip_checkpoint()
                    .ok_or_else(|| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            "current conversation has no exact completed provider tip".into(),
                        ))
                    })?
                    .to_owned();
                let undo_recovery = prepare_recovery(
                    &self.store,
                    HistoryRecoveryView {
                        recovery_id: format!("restore:{command_id}"),
                        task_id: task_id.to_string(),
                        source_binding_id: binding.binding_id.clone(),
                        source_provider_session_id: binding.provider_task_id.clone(),
                        cutoff_message_id: recovery_view.cutoff_message_id.clone(),
                        source_tip_checkpoint_id: current_tip_checkpoint_id,
                        created_at_ms: crate::now_ms(),
                        summary: "Conversation history before restoring an earlier recovery. Files and external effects were not rewound."
                            .into(),
                    },
                )
                .map_err(TaskProjectionError::Mutation)?;
                let provider_result = self
                    .fork_provider_history(
                        command_id,
                        &source,
                        &old_binding,
                        zeron_proto::ProviderForkBoundary::ThroughCheckpoint {
                            checkpoint_id: recovery_view.source_tip_checkpoint_id.clone(),
                        },
                        false,
                    )
                    .await?;
                let provider_session_id = provider_result.provider_session_id.clone();
                if provider_session_id == old_binding.provider_task_id
                    || provider_session_id == binding.provider_task_id
                {
                    return Err(TaskProjectionError::ProviderUnknown(
                        crate::EngineError::Other(
                            "provider recovery did not create an independent session".into(),
                        ),
                    ));
                }
                let remapped_checkpoints = plan
                    .remapped_checkpoints(&provider_result)
                    .map_err(TaskProjectionError::ProviderUnknown)?;

                let after = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            error.to_string(),
                        ))
                    })?;
                let (_, current_binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                if before.task_revision != after.task_revision
                    || before.task_sequence != after.task_sequence
                    || binding.binding_id != current_binding.binding_id
                {
                    return Err(TaskProjectionError::ProviderUnknown(
                        crate::EngineError::Other(
                            "task changed while history recovery was being prepared".into(),
                        ),
                    ));
                }
                let cwd = source.cwd.clone().unwrap_or_else(|| ".".into());
                self.sessions.retire_context_binding(task_id, &cwd);
                let plan_entries = plan.entries;
                let plan_checkpoints = remapped_checkpoints;
                let marker = self.history_marker(
                    format!("history-restore:{command_id}"),
                    format!(
                        "Restored conversation recovery {recovery_id}. Files, tools, and external effects were not undone."
                    ),
                );
                let task_for_registry = task_id.to_string();
                let provider_for_registry = provider_session_id.clone();
                let receipt = self
                    .task_identity
                    .replace_with_forked_binding_with_command(
                        task_id,
                        &provider_session_id,
                        |new_binding, identity_snapshot| {
                            let extra = [
                                (
                                    undo_recovery.index_id.as_str(),
                                    undo_recovery.index_bytes.as_slice(),
                                ),
                                (
                                    undo_recovery.snapshot_id.as_str(),
                                    current_snapshot.as_slice(),
                                ),
                            ];
                            let (_, receipt) = self.doc_host.commit_context_boundary(
                                task_id,
                                command_id,
                                Some(&new_binding.binding_id),
                                Some(new_binding.generation),
                                identity_snapshot,
                                None,
                                &extra,
                                move |doc| {
                                    doc.replace_conversation_entries(
                                        &plan_entries,
                                        &plan_checkpoints,
                                    )?;
                                    doc.push_message(&marker)?;
                                    let mut context = ContextControlState::default();
                                    context.binding_generation = Some(new_binding.generation);
                                    context.notice = Some(
                                        "Context controls reset for the restored provider binding."
                                            .into(),
                                    );
                                    doc.set_context_control(&context)?;
                                    Ok(())
                                },
                                move |registry| {
                                    if !registry.set_chat_harness_session(
                                        &task_for_registry,
                                        &provider_for_registry,
                                        &cwd,
                                    )? {
                                        return Err(crate::EngineError::Other(
                                            "task vanished during history recovery".into(),
                                        ));
                                    }
                                    Ok(())
                                },
                            )?;
                            Ok(receipt)
                        },
                    )
                    .map_err(|error| {
                        TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            error.message().to_string(),
                        ))
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::Rename { title } => {
                let native = self.task_identity.has_native_codex_binding(task_id);
                if native {
                    self.task_identity
                        .reconcile_task(task_id, self.sessions.has_live_run(task_id))
                        .await
                        .map_err(TaskProjectionError::Mutation)?;
                    self.task_identity
                        .set_provider_name(task_id, &title)
                        .await
                        .map_err(TaskProjectionError::Mutation)?;
                }
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        doc.rename_chat(&owned_task_id, &title)?;
                        Ok(())
                    })?;
                if native
                    && let Err(error) = self
                        .task_identity
                        .reconcile_task(task_id, self.sessions.has_live_run(task_id))
                        .await
                {
                    tracing::warn!(task = %task_id, %error, "post-rename task reconciliation failed");
                }
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetPinned {
                pinned,
                after_task_id,
                before_task_id,
            } => {
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        if !pinned {
                            doc.set_chat_pinned_order(&owned_task_id, None)?;
                            return Ok(());
                        }
                        let mut pinned: Vec<_> = doc
                            .read_chats()?
                            .into_iter()
                            .filter(|chat| {
                                !chat.archived
                                    && chat.id != owned_task_id
                                    && chat.pinned_order.is_some()
                            })
                            .collect();
                        pinned.sort_by(|left, right| {
                            zeron_proto::compare_pin_order(
                                left.pinned_order.as_deref().expect("filtered pin key"),
                                right.pinned_order.as_deref().expect("filtered pin key"),
                            )
                            .then_with(|| left.id.cmp(&right.id))
                        });

                        let insertion = match (after_task_id.as_deref(), before_task_id.as_deref())
                        {
                            (Some(after), Some(before)) => {
                                let after_ix = pinned
                                    .iter()
                                    .position(|chat| chat.id == after)
                                    .ok_or_else(|| {
                                        crate::EngineError::Other(
                                            "the previous pinned task changed".into(),
                                        )
                                    })?;
                                let before_ix = pinned
                                    .iter()
                                    .position(|chat| chat.id == before)
                                    .ok_or_else(|| {
                                        crate::EngineError::Other(
                                            "the next pinned task changed".into(),
                                        )
                                    })?;
                                if before_ix != after_ix + 1 {
                                    return Err(crate::EngineError::Other(
                                        "the pinned order changed".into(),
                                    ));
                                }
                                before_ix
                            }
                            (Some(after), None) => pinned
                                .iter()
                                .position(|chat| chat.id == after)
                                .map(|ix| ix + 1)
                                .ok_or_else(|| {
                                    crate::EngineError::Other(
                                        "the previous pinned task changed".into(),
                                    )
                                })?,
                            (None, Some(before)) => pinned
                                .iter()
                                .position(|chat| chat.id == before)
                                .ok_or_else(|| {
                                    crate::EngineError::Other("the next pinned task changed".into())
                                })?,
                            (None, None) => pinned.len(),
                        };
                        let left = insertion
                            .checked_sub(1)
                            .and_then(|ix| pinned[ix].pinned_order.as_deref());
                        let right = pinned
                            .get(insertion)
                            .and_then(|chat| chat.pinned_order.as_deref());
                        let order = zeron_proto::pin_order_between(left, right)
                            .map_err(|error| crate::EngineError::Other(error.into()))?;
                        doc.set_chat_pinned_order(&owned_task_id, Some(&order))?;
                        Ok(())
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetRead { read } => {
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        if read {
                            doc.set_chat_seen(&owned_task_id, chrono::Utc::now())?;
                        } else {
                            doc.set_chat_unread(&owned_task_id, true)?;
                        }
                        Ok(())
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetArchived { archived } => {
                let native = self.task_identity.has_native_codex_binding(task_id);
                if native {
                    self.task_identity
                        .reconcile_task(task_id, self.sessions.has_live_run(task_id))
                        .await
                        .map_err(TaskProjectionError::Mutation)?;
                    self.task_identity
                        .set_provider_archived(task_id, archived)
                        .await
                        .map_err(TaskProjectionError::Mutation)?;
                }
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        doc.set_chat_archived(&owned_task_id, archived)?;
                        Ok(())
                    })?;
                if native
                    && let Err(error) = self
                        .task_identity
                        .reconcile_task(task_id, self.sessions.has_live_run(task_id))
                        .await
                {
                    tracing::warn!(task = %task_id, %error, "post-archive task reconciliation failed");
                }
                Ok(Some(receipt))
            }
            TaskCommandIntent::MoveProject {
                destination_space_id,
            } => {
                let owned_task_id = task_id.to_string();
                let validated = validated_project_move.ok_or_else(|| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "project move reached commit without a validated destination".into(),
                    ))
                })?;
                if validated.destination.id != destination_space_id {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "validated project move does not match the command destination".into(),
                    )));
                }
                let sessions = self.sessions.clone();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        // Recheck while the registry commit lock is held. This
                        // covers turns parked on approval/input as well as
                        // streaming work without introducing the later L28
                        // computer-control lease system.
                        if sessions.has_live_run(&owned_task_id) {
                            return Err(crate::EngineError::Other(
                                "the task became active before the project move committed".into(),
                            ));
                        }
                        commit_validated_project_move(doc, &owned_task_id, &validated)
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::Delete {} => {
                let owned_task_id = task_id.to_string();
                let mut current = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                current.binding_id = None;
                current.binding_generation = None;
                current.active_turn_id = None;
                let (_, receipt) = self.task_identity.tombstone_task_with_command(
                    task_id,
                    move |identity_snapshot| {
                        self.commit_task_projection_with_changes(
                            command_id,
                            &current,
                            identity_snapshot,
                            Some(task_id),
                            move |doc| {
                                doc.delete_chat(&owned_task_id)?;
                                Ok(())
                            },
                        )
                    },
                )?;
                self.doc_host.evict_chat(task_id);
                // The task tombstone is authoritative. Stop only PTYs opened
                // for this task and remove their local output history.
                let removed = self
                    .terminals
                    .delete_task(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                if removed > 0 {
                    tracing::info!(task = %task_id, records = removed, "deleted task terminal history");
                }
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetBranch { branch } => {
                let owned_task_id = task_id.to_string();
                let (_, receipt) = self.commit_task_projection_superseding_recovery(
                    command_id,
                    task_id,
                    "Recovery cancelled because the checkout branch changed.",
                    move |doc| {
                        doc.set_chat_branch(&owned_task_id, &branch)?;
                        Ok(())
                    },
                )?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetCwd { cwd } => {
                let owned_task_id = task_id.to_string();
                let (_, receipt) = self.commit_task_projection_superseding_recovery(
                    command_id,
                    task_id,
                    "Recovery cancelled because the working directory changed.",
                    move |doc| {
                        doc.set_chat_cwd(&owned_task_id, &cwd)?;
                        Ok(())
                    },
                )?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetActivity {
                last_message_at,
                created_at,
            } => {
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        let Some(mut chat) = doc.chat(&owned_task_id)? else {
                            return Ok(());
                        };
                        if let Some(ms) = last_message_at {
                            chat.last_message_at =
                                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms);
                        }
                        if let Some(ms) = created_at
                            && let Some(at) =
                                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
                        {
                            chat.created_at = at;
                        }
                        doc.upsert_chat(&chat)?;
                        Ok(())
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetHost { device_id } => {
                let owned_task_id = task_id.to_string();
                let (_, receipt) = self.commit_task_projection_superseding_recovery(
                    command_id,
                    task_id,
                    "Recovery cancelled because the owning environment changed.",
                    move |doc| {
                        let Some(mut chat) = doc.chat(&owned_task_id)? else {
                            return Ok(());
                        };
                        chat.device_id = device_id;
                        doc.upsert_chat(&chat)?;
                        Ok(())
                    },
                )?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::CompactContext {} => {
                self.apply_compact_context(command_id, task_id).await
            }
            TaskCommandIntent::ClearContext {} => {
                let config = self
                    .workspace
                    .chat(task_id)
                    .map_err(TaskProjectionError::Mutation)?
                    .and_then(|chat| chat.config)
                    .ok_or_else(|| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            "clear context requires a provider configuration".into(),
                        ))
                    })?;
                self.apply_context_boundary(
                    command_id,
                    task_id,
                    config,
                    ContextBoundaryKind::Cleared,
                    None,
                )
            }
            TaskCommandIntent::SwitchProvider { config, handoff } => self.apply_context_boundary(
                command_id,
                task_id,
                config,
                ContextBoundaryKind::ProviderSwitched,
                handoff,
            ),
            TaskCommandIntent::SetModel { config } => {
                self.apply_model_change(command_id, task_id, config)
            }
            TaskCommandIntent::SetConfig { config } => {
                if self
                    .workspace
                    .chat(task_id)
                    .map_err(TaskProjectionError::Mutation)?
                    .and_then(|chat| chat.config)
                    .is_some_and(|current| current.harness != config.harness)
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "use the explicit provider-switch action to change harnesses".into(),
                    )));
                }
                let owned_task_id = task_id.to_string();
                let (_, receipt) = self.commit_task_projection_superseding_recovery(
                    command_id,
                    task_id,
                    "Recovery cancelled because the task configuration changed.",
                    move |doc| {
                        doc.set_chat_config(&owned_task_id, &config)?;
                        Ok(())
                    },
                )?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetPermission { mode, actor } => {
                let prepared = self
                    .sessions
                    .prepare_permission_mode(task_id, mode, actor)
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                let config = prepared.config.clone();
                let owned_task_id = task_id.to_string();
                let (_, receipt) = self.commit_task_projection_superseding_recovery(
                    command_id,
                    task_id,
                    "Recovery cancelled because the permission grant changed.",
                    move |doc| {
                        doc.set_chat_config(&owned_task_id, &config)?;
                        Ok(())
                    },
                )?;
                drop(prepared);
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetProviderAccount { account_id } => {
                let prepared = self
                    .sessions
                    .prepare_task_provider_account(task_id, &account_id)
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                let config = prepared.config.clone();
                let harness = config.harness;
                let owned_task_id = task_id.to_string();
                let (_, receipt) = self.commit_task_projection_superseding_recovery(
                    command_id,
                    task_id,
                    "Recovery cancelled because the provider account changed.",
                    move |doc| {
                        doc.set_chat_config(&owned_task_id, &config)?;
                        Ok(())
                    },
                )?;
                self.sessions.invalidate_provider_account_caches(harness);
                drop(prepared);
                Ok(Some(receipt))
            }
            TaskCommandIntent::QueueMessage {
                message_id,
                text,
                attachments,
                hold_for_turn_end,
                follow_up_behavior,
                execution,
            } => {
                use crate::doc_host::QueueMutationOutcome;
                let (outcome, committed) = self
                    .doc_host
                    .enqueue_queued_message(
                        command_id,
                        task_id,
                        &message_id,
                        &text,
                        attachments,
                        hold_for_turn_end,
                        follow_up_behavior,
                        execution,
                    )
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                if let Some(receipt) = committed {
                    let accepted_turn_id = receipt.snapshot.active_turn_id.clone();
                    if matches!(outcome, QueueMutationOutcome::Applied)
                        && follow_up_behavior == FollowUpBehavior::Steer
                        && let Err(error) = self
                            .doc_host
                            .steer_queued_now_for_turn(
                                task_id,
                                &message_id,
                                accepted_turn_id.as_deref(),
                            )
                            .await
                    {
                        tracing::warn!(task = task_id, message = %message_id, %error,
                            "preferred native steer attempt stayed queued");
                    }
                    return Ok(Some(receipt));
                }
                let (status, applied, code, message, recovery) = match outcome {
                    QueueMutationOutcome::Applied | QueueMutationOutcome::Duplicate => {
                        unreachable!("applied queue mutation must commit its receipt atomically")
                    }
                    QueueMutationOutcome::RevisionConflict(_) => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::CommandIdConflict,
                        "that message id already names different queued content",
                        "Read the existing queue row; never retry this payload with another id automatically.",
                    ),
                    QueueMutationOutcome::Claimed(_) => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::QueueClaimed,
                        "the queued message is already claimed for delivery",
                        "Observe the claimed delivery; do not enqueue a duplicate.",
                    ),
                    QueueMutationOutcome::Missing => unreachable!(),
                };
                let receipt = self
                    .finish_task_command(command_id, status, applied, code, message, recovery)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::EditQueuedMessage {
                message_id,
                expected_queue_revision,
                lease_id,
                text,
                expected_text_hash,
                attachments,
            } => {
                use crate::doc_host::FinishQueueEditOutcome;
                let already_applied = self
                    .doc_host
                    .queued_message(task_id, &message_id)
                    .map_err(TaskProjectionError::Mutation)?
                    .is_some_and(|item| {
                        item.revision == expected_queue_revision.saturating_add(1)
                            && item.text == text
                            && item.attachments == attachments
                            && item.delivery_gate.is_none()
                            && matches!(item.status, zeron_doc::QueueMessageStatus::Held)
                    });
                let (outcome, committed) = if already_applied {
                    // Crash window: the queue projection committed but the
                    // L07 receipt did not. Replaying the same command proves
                    // its exact next revision/content and finalizes it without
                    // applying a second edit.
                    (FinishQueueEditOutcome::Committed, None)
                } else {
                    self.doc_host
                        .finish_queued_message_edit_task(
                            command_id,
                            task_id,
                            &message_id,
                            &lease_id,
                            &text,
                            &expected_text_hash,
                            &attachments,
                            expected_queue_revision,
                        )
                        .await
                        .map_err(TaskProjectionError::Mutation)?
                };
                if let Some(receipt) = committed {
                    return Ok(Some(receipt));
                }
                let (status, applied, code, message, recovery) = match outcome {
                    FinishQueueEditOutcome::Committed => (
                        CommandReceiptStatus::Applied,
                        true,
                        CommandReasonCode::OperationRejected,
                        "",
                        "",
                    ),
                    FinishQueueEditOutcome::RevisionConflict { .. }
                    | FinishQueueEditOutcome::Conflict { .. } => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::QueueRevisionConflict,
                        "the queued message changed after editing began",
                        "Keep the editor content, reload the row, and deliberately retry from the current revision.",
                    ),
                    FinishQueueEditOutcome::Claimed { .. } => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::QueueClaimed,
                        "the queued message was claimed before this edit committed",
                        "Keep the editor content and observe delivery; stale edits cannot replace claimed work.",
                    ),
                    FinishQueueEditOutcome::Missing => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::QueueMissing,
                        "the queued message no longer exists",
                        "Keep the editor content and reload the queue.",
                    ),
                    FinishQueueEditOutcome::Lost
                    | FinishQueueEditOutcome::Cancelled
                    | FinishQueueEditOutcome::Discarded
                    | FinishQueueEditOutcome::Released => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::OperationRejected,
                        "the queue edit lease is no longer valid",
                        "Keep the editor content and acquire a fresh edit lease.",
                    ),
                };
                let receipt = self
                    .finish_task_command(command_id, status, applied, code, message, recovery)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::RemoveQueuedMessage {
                message_id,
                expected_queue_revision,
            } => {
                use crate::doc_host::QueueMutationOutcome;
                let (outcome, committed) = self
                    .doc_host
                    .remove_queued_message_at_revision(
                        command_id,
                        task_id,
                        &message_id,
                        expected_queue_revision,
                    )
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                if let Some(receipt) = committed {
                    return Ok(Some(receipt));
                }
                let (status, applied, code, message, recovery) = match outcome {
                    QueueMutationOutcome::Applied | QueueMutationOutcome::Missing => {
                        unreachable!("applied queue removal must commit its receipt atomically")
                    }
                    QueueMutationOutcome::RevisionConflict(_) => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::QueueRevisionConflict,
                        "the queued message changed before removal",
                        "Reload the queue and deliberately remove the current revision if still wanted.",
                    ),
                    QueueMutationOutcome::Claimed(_) => (
                        CommandReceiptStatus::Conflict,
                        false,
                        CommandReasonCode::QueueClaimed,
                        "the queued message was already claimed for delivery",
                        "Observe or stop the active delivery; stale removal cannot cancel a claimed message.",
                    ),
                    QueueMutationOutcome::Duplicate => unreachable!(),
                };
                let receipt = self
                    .finish_task_command(command_id, status, applied, code, message, recovery)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SendQueuedNow {
                message_id,
                expected_queue_revision,
            } => {
                let revision = match expected_queue_revision {
                    Some(revision) => revision,
                    None => self
                        .doc_host
                        .queued_message(task_id, &message_id)
                        .map_err(TaskProjectionError::Mutation)?
                        .map(|item| item.revision)
                        .unwrap_or(0),
                };
                match self
                    .doc_host
                    .send_queued_now_with_claim(
                        task_id,
                        &message_id,
                        revision,
                        command_id,
                        expected_turn_id,
                    )
                    .await
                    .map_err(TaskProjectionError::Mutation)?
                {
                    crate::doc_host::QueueDispatchOutcome::Started
                    | crate::doc_host::QueueDispatchOutcome::AlreadyClaimed(_)
                    | crate::doc_host::QueueDispatchOutcome::Rejected(_)
                    | crate::doc_host::QueueDispatchOutcome::Unknown(_) => Ok(None),
                    crate::doc_host::QueueDispatchOutcome::RevisionConflict(_) => {
                        let receipt = self.finish_task_command(
                            command_id,
                            CommandReceiptStatus::Conflict,
                            false,
                            CommandReasonCode::QueueRevisionConflict,
                            "the queued message changed before send-now claimed it",
                            "Reload the queue before deliberately sending the current revision.",
                        ).map_err(|error| TaskProjectionError::Commit(crate::EngineError::Other(error.to_string())))?;
                        Ok(Some(receipt))
                    }
                    crate::doc_host::QueueDispatchOutcome::Blocked(_) => {
                        let receipt = self
                            .finish_task_command(
                                command_id,
                                CommandReceiptStatus::Conflict,
                                false,
                                CommandReasonCode::QueueClaimed,
                                "the queued message is being edited, reviewed, or delivered",
                                "Finish the current queue action and reload before retrying.",
                            )
                            .map_err(|error| {
                                TaskProjectionError::Commit(crate::EngineError::Other(
                                    error.to_string(),
                                ))
                            })?;
                        Ok(Some(receipt))
                    }
                    crate::doc_host::QueueDispatchOutcome::Missing => {
                        let receipt = self
                            .finish_task_command(
                                command_id,
                                CommandReceiptStatus::Conflict,
                                false,
                                CommandReasonCode::QueueMissing,
                                "the queued message no longer exists",
                                "Reload the queue; do not construct a replacement automatically.",
                            )
                            .map_err(|error| {
                                TaskProjectionError::Commit(crate::EngineError::Other(
                                    error.to_string(),
                                ))
                            })?;
                        Ok(Some(receipt))
                    }
                }
            }
            TaskCommandIntent::SteerQueuedNow {
                message_id,
                expected_queue_revision,
            } => {
                let revision = match expected_queue_revision {
                    Some(revision) => revision,
                    None => self
                        .doc_host
                        .queued_message(task_id, &message_id)
                        .map_err(TaskProjectionError::Mutation)?
                        .map(|item| item.revision)
                        .unwrap_or(0),
                };
                match self
                    .doc_host
                    .steer_queued_now_with_claim(
                        task_id,
                        &message_id,
                        revision,
                        command_id,
                        expected_turn_id,
                    )
                    .await
                    .map_err(TaskProjectionError::Mutation)?
                {
                    crate::doc_host::QueueDispatchOutcome::Started
                    | crate::doc_host::QueueDispatchOutcome::AlreadyClaimed(_)
                    | crate::doc_host::QueueDispatchOutcome::Rejected(_)
                    | crate::doc_host::QueueDispatchOutcome::Unknown(_) => Ok(None),
                    crate::doc_host::QueueDispatchOutcome::RevisionConflict(_) => {
                        let receipt = self.finish_task_command(
                            command_id,
                            CommandReceiptStatus::Conflict,
                            false,
                            CommandReasonCode::QueueRevisionConflict,
                            "the queued message changed before steer-now claimed it",
                            "Reload the queue before deliberately steering the current revision.",
                        ).map_err(|error| TaskProjectionError::Commit(crate::EngineError::Other(error.to_string())))?;
                        Ok(Some(receipt))
                    }
                    crate::doc_host::QueueDispatchOutcome::Blocked(_) => {
                        let receipt = self
                            .finish_task_command(
                                command_id,
                                CommandReceiptStatus::Conflict,
                                false,
                                CommandReasonCode::QueueClaimed,
                                "the queued message is being edited, reviewed, or delivered",
                                "Finish the current queue action and reload before retrying.",
                            )
                            .map_err(|error| {
                                TaskProjectionError::Commit(crate::EngineError::Other(
                                    error.to_string(),
                                ))
                            })?;
                        Ok(Some(receipt))
                    }
                    crate::doc_host::QueueDispatchOutcome::Missing => {
                        let receipt = self
                            .finish_task_command(
                                command_id,
                                CommandReceiptStatus::Conflict,
                                false,
                                CommandReasonCode::QueueMissing,
                                "the queued message no longer exists",
                                "Reload the queue; do not construct a replacement automatically.",
                            )
                            .map_err(|error| {
                                TaskProjectionError::Commit(crate::EngineError::Other(
                                    error.to_string(),
                                ))
                            })?;
                        Ok(Some(receipt))
                    }
                }
            }
            TaskCommandIntent::RetryRecovery { chain_id } => {
                let Some(change) = self
                    .sessions
                    .prepare_recovery_attempt(task_id, &chain_id)
                    .map_err(TaskProjectionError::Mutation)?
                else {
                    let receipt = self
                        .finish_task_command(
                            command_id,
                            CommandReceiptStatus::Rejected,
                            false,
                            CommandReasonCode::OperationRejected,
                            "the recovery chain is no longer active or replay-safe",
                            "Reload task state before choosing another recovery action.",
                        )
                        .map_err(|error| {
                            TaskProjectionError::Commit(crate::EngineError::Other(
                                error.to_string(),
                            ))
                        })?;
                    return Ok(Some(receipt));
                };
                let current = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                let transition = change
                    .storage_transition()
                    .map_err(TaskProjectionError::Mutation)?;
                let receipt = self
                    .doc_host
                    .complete_task_command_with_recovery(
                        command_id,
                        CommandReceiptStatus::Applied,
                        true,
                        current.binding_id.as_deref(),
                        current.binding_generation,
                        current.active_turn_id.as_deref(),
                        None,
                        None,
                        &transition,
                    )
                    .map_err(TaskProjectionError::Commit)?
                    .ok_or_else(|| {
                        TaskProjectionError::Commit(crate::EngineError::Other(
                            "task command receipt vanished during recovery claim".into(),
                        ))
                    })?;
                self.sessions
                    .dispatch_prepared_recovery(change)
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::CancelRecovery { chain_id } => {
                let Some(change) = self
                    .sessions
                    .prepare_cancel_recovery(task_id, &chain_id)
                    .map_err(TaskProjectionError::Mutation)?
                else {
                    let receipt = self
                        .finish_task_command(
                            command_id,
                            CommandReceiptStatus::Rejected,
                            false,
                            CommandReasonCode::OperationRejected,
                            "the recovery chain is no longer active",
                            "Reload task recovery state.",
                        )
                        .map_err(|error| {
                            TaskProjectionError::Commit(crate::EngineError::Other(
                                error.to_string(),
                            ))
                        })?;
                    return Ok(Some(receipt));
                };
                let current = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                let transition = change
                    .storage_transition()
                    .map_err(TaskProjectionError::Mutation)?;
                let receipt = self
                    .doc_host
                    .complete_task_command_with_recovery(
                        command_id,
                        CommandReceiptStatus::Applied,
                        true,
                        current.binding_id.as_deref(),
                        current.binding_generation,
                        current.active_turn_id.as_deref(),
                        None,
                        None,
                        &transition,
                    )
                    .map_err(TaskProjectionError::Commit)?
                    .ok_or_else(|| {
                        TaskProjectionError::Commit(crate::EngineError::Other(
                            "task command receipt vanished during recovery cancellation".into(),
                        ))
                    })?;
                self.sessions.publish_committed_recovery(&change.next);
                Ok(Some(receipt))
            }
            TaskCommandIntent::MarkSeen { at } => {
                let at = at
                    .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                    .unwrap_or_else(chrono::Utc::now);
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        doc.set_chat_seen(&owned_task_id, at)?;
                        Ok(())
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::Session { .. } => unreachable!("session handled by command queue"),
        }
    }
}

fn canonical_payload_hash(intent: &TaskCommandIntent) -> Result<String, serde_json::Error> {
    fn canonical(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Null => out.push_str("null"),
            serde_json::Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            serde_json::Value::Number(value) => out.push_str(&value.to_string()),
            serde_json::Value::String(value) => {
                out.push_str(&serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()));
            }
            serde_json::Value::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    canonical(value, out);
                }
                out.push(']');
            }
            serde_json::Value::Object(values) => {
                out.push('{');
                let sorted = values.iter().collect::<BTreeMap<_, _>>();
                for (index, (key, value)) in sorted.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()));
                    out.push(':');
                    canonical(value, out);
                }
                out.push('}');
            }
        }
    }
    let value = serde_json::to_value(intent)?;
    let mut encoded = String::new();
    canonical(&value, &mut encoded);
    let digest = Sha256::digest(encoded.as_bytes());
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// An RPC rejection is scoped to the requested capability. Only a broken
/// transport means the shared device link itself cannot carry other calls.
fn should_invalidate_link(error: &RpcError) -> bool {
    matches!(error, RpcError::Closed | RpcError::Transport(_))
}

fn legacy_task_command(task_id: String, intent: TaskCommandIntent) -> TaskCommandParams {
    TaskCommandParams {
        command_id: crate::new_id(),
        actor_id: "legacy-local-user".into(),
        client_id: "legacy-local-rpc".into(),
        client_session_id: "legacy".into(),
        task_id,
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
        issued_at_ms: crate::now_ms(),
        intent,
    }
}

fn task_intent_from_mutation(mutation: MutateParams) -> Option<(String, TaskCommandIntent)> {
    Some(match mutation {
        MutateParams::CreateChat {
            chat_id,
            space_id,
            device_id,
            config,
            branch,
            cwd,
            lineage,
        } => (
            chat_id,
            TaskCommandIntent::CreateChat {
                space_id,
                device_id,
                config,
                branch,
                cwd,
                lineage,
            },
        ),
        MutateParams::RenameChat { chat_id, title } => {
            (chat_id, TaskCommandIntent::Rename { title })
        }
        MutateParams::SetChatBranch { chat_id, branch } => {
            (chat_id, TaskCommandIntent::SetBranch { branch })
        }
        MutateParams::SetChatCwd { chat_id, cwd } => (chat_id, TaskCommandIntent::SetCwd { cwd }),
        MutateParams::SetChatActivity {
            chat_id,
            last_message_at,
            created_at,
        } => (
            chat_id,
            TaskCommandIntent::SetActivity {
                last_message_at,
                created_at,
            },
        ),
        MutateParams::SetChatHost { chat_id, device_id } => {
            (chat_id, TaskCommandIntent::SetHost { device_id })
        }
        MutateParams::SetChatArchived { chat_id, archived } => {
            (chat_id, TaskCommandIntent::SetArchived { archived })
        }
        MutateParams::SetChatConfig { chat_id, config } => {
            (chat_id, TaskCommandIntent::SetConfig { config })
        }
        MutateParams::SetThreadPermissionMode {
            chat_id,
            mode,
            actor,
        } => (chat_id, TaskCommandIntent::SetPermission { mode, actor }),
        MutateParams::SetThreadProviderAccount {
            chat_id,
            account_id,
        } => (
            chat_id,
            TaskCommandIntent::SetProviderAccount { account_id },
        ),
        MutateParams::DeleteChat { chat_id } => (chat_id, TaskCommandIntent::Delete {}),
        MutateParams::MarkChatSeen { chat_id, at } => (chat_id, TaskCommandIntent::MarkSeen { at }),
        MutateParams::CreateSpace { .. }
        | MutateParams::RenameSpace { .. }
        | MutateParams::DeleteSpace { .. }
        | MutateParams::RenameDevice { .. } => return None,
    })
}

/// Reply deadline for a relay-forwarded unary call. The relay is WebSocket
/// frames through a DO: a dropped frame (host socket replaced mid-call, DO
/// restart) loses the reply SILENTLY — the DO's auto-pong keeps the client
/// socket looking healthy — and an unbounded await wedged callers forever
/// (the composer's permanent "Sending…", 2026-08-18). Network-bound git and
/// update methods get a long leash; worktree creation checks out a full tree;
/// everything else is interactive and must fail fast.
fn forward_deadline(method: &str) -> std::time::Duration {
    use std::time::Duration;
    match method {
        methods::CLONE_REPO | methods::FETCH_ALL | methods::APPLY_UPDATE => {
            Duration::from_secs(15 * 60)
        }
        methods::EXECUTE_GIT_ACTION => Duration::from_secs(150),
        methods::GENERATE_GIT_COMMIT_MESSAGE => Duration::from_secs(45),
        methods::GET_GIT_PR_STATUS => Duration::from_secs(60),
        methods::CREATE_WORKTREE => Duration::from_secs(120),
        _ => Duration::from_secs(30),
    }
}

/// ControlRpc methods that honor `targetDeviceId` (feature-inventory §2.1). Extend this
/// list (plus [`is_stream_method`] for streams) to make more of the surface
/// device-addressable — the handlers themselves need no changes.
fn forwardable(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_HARNESSES
            | methods::GET_TITLE_SETTINGS
            | methods::SET_TITLE_SETTINGS
            | methods::SET_HARNESS_ENABLED
            | methods::LIST_MODELS
            | methods::LIST_COMMANDS
            | methods::QUEUE_COMMAND
            | methods::EXECUTE_TASK_COMMAND
            | methods::GET_TASK_COMMAND_RECEIPT
            | methods::GET_TASK_COMMAND_SNAPSHOT
            | methods::WATCH_TASK_COMMANDS
            | methods::SEARCH_SESSION_HISTORY
            | methods::GET_USAGE_SNAPSHOT
            | methods::REBUILD_SESSION_HISTORY
            | methods::WATCH_DOC_MESSAGES
            // The queue lives on the chat doc, and only its host may send from
            // it — same addressing as the command ledger next door.
            | methods::WATCH_QUEUE
            | methods::QUEUE_MESSAGE
            | methods::UPDATE_QUEUED_MESSAGE
            | methods::BEGIN_QUEUED_MESSAGE_EDIT
            | methods::RENEW_QUEUED_MESSAGE_EDIT
            | methods::FINISH_QUEUED_MESSAGE_EDIT
            | methods::MOVE_QUEUED_MESSAGE
            | methods::REMOVE_QUEUED_MESSAGE
            | methods::SEND_QUEUED_MESSAGE_NOW
            | methods::STEER_QUEUED_MESSAGE_NOW
            // Repos/worktrees/folders are device-local filesystem state.
            | methods::LIST_REPOS
            | methods::ADD_REPO
            | methods::CLONE_REPO
            | methods::CREATE_REPO
            | methods::LIST_BRANCHES
            | methods::LIST_REFS
            | methods::LIST_GIT_HISTORY
            | methods::SEARCH_GIT_HISTORY
            | methods::RESOLVE_GIT_AVATARS
            | methods::FETCH_ALL
            | methods::SWITCH_REF
            | methods::LIST_FOLDERS
            | methods::LIST_DRIVES
            | methods::SEARCH_FILES
            | methods::LIST_WORKSPACE_DIRECTORY
            | methods::SEARCH_WORKSPACE_FILES
            | methods::READ_WORKSPACE_IMAGE
            | methods::READ_WORKSPACE_FILE
            | methods::WRITE_WORKSPACE_FILE
            | methods::WATCH_WORKSPACE_FILES
            | methods::CREATE_WORKTREE
            | methods::DELETE_WORKTREE
            // Checkout diffs are produced on the device holding the checkout.
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::GET_CHECKOUT_DIFF
            | methods::GET_CHECKOUT_FILE_DIFF_TEXT
            | methods::GET_GIT_ACTION_PREVIEW
            | methods::EXECUTE_GIT_ACTION
            | methods::GENERATE_GIT_COMMIT_MESSAGE
            | methods::GET_GIT_PR_STATUS
            // Terminals live on the chat's host device.
            | methods::OPEN_TERMINAL
            | methods::SUBSCRIBE_TERMINAL
            | methods::WRITE_TERMINAL
            | methods::RESIZE_TERMINAL
            | methods::CLOSE_TERMINAL
            | methods::LIST_TERMINALS
            // Provider account metadata may be inspected per device. Login,
            // activation, forgetting, polling, and cancellation remain local
            // until L29-L31 establish paired authenticated E2EE transport.
            | methods::LIST_AGENT_ACCOUNTS
            | methods::GET_RUNTIME_CONTRACT
            // Uploads/attachments target the chat's host device (the agent reads
            // the committed file from that device's disk).
            | methods::UPLOAD_CHUNK
            | methods::UPLOAD_COMMIT
            | methods::READ_ATTACHMENT_CHUNK
            // Updates report/apply on the device whose binary they concern.
            | methods::UPDATE_STATUS
            | methods::APPLY_UPDATE
    )
}

fn local_only_credential_method(method: &str) -> bool {
    matches!(
        method,
        methods::GET_BACKUP_STATUS
            | methods::CONFIGURE_BACKUP
            | methods::CONFIGURE_BACKUP_KEY
            | methods::BACKUP_NOW
            | methods::EXPORT_BACKUP
            | methods::INSPECT_BACKUP
            | methods::STAGE_BACKUP_RESTORE
            | methods::LIST_CREDENTIAL_METADATA
            | methods::SIGN_IN
            | methods::SIGN_IN_HEADLESS
            | methods::COMPLETE_SIGN_IN
            | methods::SIGN_OUT
            | methods::ACTIVATE_AGENT_ACCOUNT
            | methods::RENAME_AGENT_ACCOUNT
            | methods::FORGET_AGENT_ACCOUNT
            | methods::START_AGENT_LOGIN
            | methods::COMPLETE_AGENT_LOGIN
            | methods::POLL_AGENT_LOGIN
            | methods::CANCEL_AGENT_LOGIN
    )
}

/// Forwardable methods whose reply is a stream (proxied item-by-item).
fn is_stream_method(method: &str) -> bool {
    matches!(
        method,
        methods::WATCH_TASK_COMMANDS
            | methods::WATCH_DOC_MESSAGES
            | methods::WATCH_QUEUE
            | methods::SUBSCRIBE_TERMINAL
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::WATCH_WORKSPACE_FILES
            | methods::UPDATE_STATUS
    )
}

/// A watch receiver as a stream: current value first, then every change.
fn watch_stream<T>(rx: watch::Receiver<T>) -> BoxStream<'static, serde_json::Value>
where
    T: serde::Serialize + Clone + Send + Sync + 'static,
{
    futures::stream::unfold((rx, false), |(mut rx, emitted)| async move {
        if emitted {
            rx.changed().await.ok()?;
        }
        let value = {
            let borrowed = rx.borrow_and_update();
            serde_json::to_value(&*borrowed).ok()?
        };
        Some((value, (rx, true)))
    })
    .boxed()
}

/// The transcript watch as delta frames (`zeron_doc::transcript_delta`): a
/// full `reset` first, then only changed entries per commit — the whole-Vec
/// serialization here was the per-tick cost that scaled with transcript size.
fn doc_messages_stream(
    rx: watch::Receiver<crate::doc_host::TranscriptSnapshot>,
    doc: std::sync::Arc<zeron_doc::SessionDoc>,
) -> BoxStream<'static, serde_json::Value> {
    use zeron_doc::transcript_delta::{TranscriptFrame, diff_transcript};
    futures::stream::unfold(
        (
            rx,
            None::<crate::doc_host::TranscriptSnapshot>,
            doc,
            None,
            zeron_proto::ContextControlState::default(),
            zeron_doc::TranscriptBaseline::default(),
        ),
        |(
            mut rx,
            mut prev,
            doc,
            mut previous_usage,
            mut previous_context,
            mut opening_baseline,
        )| async move {
            loop {
                if prev.is_some() {
                    rx.changed().await.ok()?;
                }
                // Watchers retain the immutable published snapshot. Each
                // connection used to deep-copy the entire transcript here.
                let current = rx.borrow_and_update().clone();
                let frame = match prev.as_ref() {
                    None => TranscriptFrame::reset(&current.entries),
                    Some(prev) => diff_transcript(&prev.entries, &current.entries),
                };
                let replay_baseline = match prev.as_ref() {
                    None => {
                        opening_baseline = zeron_doc::TranscriptBaseline::capture(&current.entries);
                        Some(opening_baseline.clone())
                    }
                    Some(prev)
                        if !std::sync::Arc::ptr_eq(
                            &prev.replay_baseline,
                            &current.replay_baseline,
                        ) =>
                    {
                        // The tracker only observes changes after attach; its
                        // baseline omits unchanged cached parts. Preserve this
                        // subscription's opening cutoff without capturing live
                        // appends or sharing another viewer's later cutoff.
                        // Ordinary live updates never rebuild this metadata.
                        let mut baseline = (*current.replay_baseline).clone();
                        for (entry, parts) in &opening_baseline.entries {
                            let merged = baseline.entries.entry(entry.clone()).or_default();
                            for (part, &len) in parts {
                                let cutoff = merged.entry(part.clone()).or_default();
                                *cutoff = (*cutoff).max(len);
                            }
                        }
                        Some(baseline)
                    }
                    _ => None,
                };
                prev = Some(current);
                // No-op commits (a second watcher attaching, command-only
                // changes) produce empty deltas — skip the frame entirely.
                let usage = doc.context_usage();
                let context_control = doc.context_control();
                if frame.is_empty_delta()
                    && usage == previous_usage
                    && context_control == previous_context
                    && replay_baseline.is_none()
                {
                    continue;
                }
                previous_usage = usage;
                previous_context = context_control.clone();
                let value = serde_json::to_value(zeron_doc::TranscriptUpdate {
                    frame,
                    context_usage: usage,
                    context_control,
                    replay_baseline,
                })
                .ok()?;
                return Some((
                    value,
                    (
                        rx,
                        prev,
                        doc,
                        previous_usage,
                        previous_context,
                        opening_baseline,
                    ),
                ));
            }
        },
    )
    .boxed()
}

/// Authentication-only RPC surface used while the headed app is waiting for a
/// production WorkOS session. Keeping this independent from [`EngineRpc`] lets
/// the UI show its sign-in and organization gates before identity-scoped Loro
/// stores are opened.
#[derive(Clone)]
pub struct AuthRpc {
    auth: Auth,
}

impl AuthRpc {
    pub fn new(auth: Auth) -> Self {
        Self { auth }
    }

    pub fn handles(method: &str) -> bool {
        matches!(
            method,
            methods::AUTH_STATUS
                | methods::LIST_CREDENTIAL_METADATA
                | methods::SIGN_IN
                | methods::SIGN_IN_HEADLESS
                | methods::COMPLETE_SIGN_IN
                | methods::SIGN_OUT
                | methods::LIST_ORGS
                | methods::CREATE_ORG
                | methods::SELECT_ORG
        )
    }
}

#[async_trait]
impl RpcService for AuthRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::AUTH_STATUS => Ok(RpcReply::Stream(watch_stream(self.auth.watch_state()))),
            methods::LIST_CREDENTIAL_METADATA => RpcReply::value(
                &self
                    .auth
                    .credential_metadata()
                    .map_err(|error| RpcError::Failed(error.to_string()))?,
            ),
            methods::SIGN_IN => {
                let url = self
                    .auth
                    .start_sign_in()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            methods::SIGN_IN_HEADLESS => {
                let url = self.auth.start_headless_sign_in();
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            methods::COMPLETE_SIGN_IN => {
                #[derive(Deserialize)]
                struct P {
                    code: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .complete_sign_in(&p.code)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SIGN_OUT => {
                self.auth
                    .sign_out()
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LIST_ORGS => {
                let orgs = self
                    .auth
                    .list_orgs()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "orgs": orgs }))
            }
            methods::CREATE_ORG => {
                #[derive(Deserialize)]
                struct P {
                    name: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .create_org(&p.name)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SELECT_ORG => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    organization_id: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .select_org(&p.organization_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            _ => Err(RpcError::UnknownMethod(method.to_string())),
        }
    }
}

fn activity_metadata_fingerprint(chat: &Chat) -> String {
    serde_json::to_string(&(
        &chat.device_id,
        &chat.space_id,
        &chat.cwd,
        &chat.checkout_id,
        &chat.source_context,
        &chat.config,
        &chat.harness_session_id,
        &chat.last_message_at,
    ))
    .unwrap_or_else(|_| format!("{}:{}", chat.device_id, chat.id))
}

fn task_command_history(
    store: &DocsStore,
    task_id: &str,
) -> Result<(Vec<TaskCommandEvent>, bool), zeron_sync::StoreError> {
    const MAX_EVENTS: usize = 10_000;
    let mut cursor = 0;
    let mut events = Vec::new();
    let mut truncated = false;
    loop {
        let page = store.read_task_commands(task_id, cursor, 100)?;
        if page.reload_required {
            break;
        }
        events.extend(page.events);
        if events.len() >= MAX_EVENTS {
            truncated = page.has_more;
            break;
        }
        if !page.has_more {
            break;
        }
        let Some(next) = page.next_task_sequence else {
            break;
        };
        cursor = next;
    }
    events.truncate(MAX_EVENTS);
    Ok((events, truncated))
}

fn index_activity_task(
    store: &DocsStore,
    identity: &crate::TaskIdentityService,
    sessions: &SessionsEngine,
    local_device_id: &str,
    chat: &Chat,
    entries: Vec<SessionMessageEntry>,
) -> Result<usize, String> {
    let _activity_guard = store.activity_index_guard();
    let policy = chat
        .space_id
        .as_deref()
        .map(|project_id| store.activity_project_policy(project_id))
        .transpose()
        .map_err(|error| error.to_string())?
        .flatten();
    let (receipts, receipts_truncated) =
        task_command_history(store, &chat.id).map_err(|error| error.to_string())?;
    let task_identity = identity.view(&chat.id);
    let run_metadata = sessions.activity_run_metadata(&chat.id);
    let events = crate::activity_history::build_activity_envelopes(
        local_device_id,
        chat,
        task_identity.as_ref(),
        &entries,
        &receipts,
        receipts_truncated,
        &run_metadata,
        policy.as_ref(),
    );
    let count = events.len();
    store
        .replace_activity_events(&chat.id, &events)
        .map_err(|error| error.to_string())?;
    Ok(count)
}

fn cross_task_summary(
    chat: &Chat,
    session: Option<&zeron_proto::Session>,
) -> zeron_proto::CrossTaskSummary {
    let live = session.filter(|session| {
        session.device_id == chat.device_id
            && session.updated_at.timestamp_millis().saturating_add(45_000) >= crate::now_ms()
    });
    let indicator = zeron_proto::chat_indicator(chat, live);
    let status = serde_json::to_value(indicator)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    zeron_proto::CrossTaskSummary {
        task_id: chat.id.clone(),
        project_id: chat.space_id.clone(),
        environment_id: chat.device_id.clone(),
        title: chat.title.clone().unwrap_or_else(|| "Untitled task".into()),
        status,
        archived: chat.archived,
        updated_at_ms: chat
            .last_message_at
            .map(|date| date.timestamp_millis())
            .unwrap_or_else(|| chat.created_at.timestamp_millis())
            .max(
                live.map(|session| session.updated_at.timestamp_millis())
                    .unwrap_or(i64::MIN),
            ),
    }
}

fn cross_task_queue_command(
    request: &zeron_proto::CrossTaskSendRequest,
    authority: &CrossTaskAuthority,
) -> TaskCommandParams {
    TaskCommandParams {
        command_id: request.idempotency_key.clone(),
        actor_id: format!("luna-tool:{}", authority.caller_task_id),
        client_id: "luna-cross-task".into(),
        client_session_id: authority.caller_task_id.clone(),
        task_id: request.task_id.clone(),
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
        issued_at_ms: crate::now_ms(),
        intent: TaskCommandIntent::QueueMessage {
            message_id: request.idempotency_key.clone(),
            text: request.message.clone(),
            attachments: Vec::new(),
            hold_for_turn_end: true,
            follow_up_behavior: FollowUpBehavior::Queue,
            execution: None,
        },
    }
}

fn cross_task_control_command(
    request: &zeron_proto::CrossTaskControlRequest,
    authority: &CrossTaskAuthority,
) -> TaskCommandParams {
    use zeron_proto::CrossTaskControlAction as Action;
    let intent = match request.action {
        Action::Create => TaskCommandIntent::CreateChat {
            space_id: Some(request.project_id.clone()),
            device_id: None,
            config: None,
            branch: None,
            cwd: None,
            lineage: TaskLineage::default(),
        },
        Action::Interrupt => TaskCommandIntent::Session {
            command: SessionCommandPayload::Interrupt {},
            transfers: Vec::new(),
        },
        Action::Archive | Action::Unarchive => TaskCommandIntent::SetArchived {
            archived: request.action == Action::Archive,
        },
        Action::Move => TaskCommandIntent::MoveProject {
            destination_space_id: request
                .destination_project_id
                .clone()
                .expect("validated move destination"),
        },
    };
    TaskCommandParams {
        command_id: request.command_id.clone(),
        actor_id: format!("luna-tool:{}", authority.caller_task_id),
        client_id: "luna-cross-task".into(),
        client_session_id: authority.caller_task_id.clone(),
        task_id: request.task_id.clone(),
        expected_task_revision: request.expected_task_revision,
        expected_binding_generation: request.expected_binding_generation,
        expected_turn_id: request.expected_turn_id.clone(),
        issued_at_ms: crate::now_ms(),
        intent,
    }
}

#[async_trait]
impl CrossTaskBackend for EngineRpc {
    fn capabilities(&self) -> CrossTaskCapabilities {
        CrossTaskCapabilities {
            list: true,
            read: true,
            durable_queue: true,
            event_wait: true,
        }
    }

    async fn task(
        &self,
        task_id: &str,
    ) -> Result<Option<zeron_proto::CrossTaskSummary>, CrossTaskError> {
        let Some(identity) = self.task_identity.view(task_id) else {
            return Ok(None);
        };
        if identity.task.tombstoned
            || identity.workspace.environment_id != self.engine_info.device_id
        {
            return Ok(None);
        }
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(|_| CrossTaskError::Backend("task lookup failed".into()))?;
        let Some(chat) = chat.filter(|chat| chat.device_id == self.engine_info.device_id) else {
            return Ok(None);
        };
        let sessions = self
            .workspace
            .read_sessions()
            .map_err(|_| CrossTaskError::Backend("status lookup failed".into()))?;
        Ok(Some(cross_task_summary(
            &chat,
            sessions.iter().find(|session| session.chat_id == task_id),
        )))
    }

    async fn list(
        &self,
        request: zeron_proto::CrossTaskListRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<zeron_proto::CrossTaskListPage, CrossTaskError> {
        let managed = self
            .task_identity
            .managed_task_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        let sessions = self
            .workspace
            .read_sessions()
            .map_err(|_| CrossTaskError::Backend("status lookup failed".into()))?;
        let mut tasks = self
            .workspace
            .read_chats()
            .map_err(|_| CrossTaskError::Backend("task list failed".into()))?
            .into_iter()
            .filter(|chat| {
                managed.contains(&chat.id) && chat.device_id == self.engine_info.device_id
            })
            .filter(|chat| {
                self.task_identity.view(&chat.id).is_some_and(|identity| {
                    !identity.task.tombstoned
                        && identity.workspace.environment_id == self.engine_info.device_id
                })
            })
            .filter(|chat| {
                chat.space_id
                    .as_ref()
                    .is_some_and(|id| authority.visible_project_ids.contains(id))
                    || chat.space_id.is_none() && authority.allow_projectless
            })
            .filter(|chat| {
                request
                    .project_id
                    .as_ref()
                    .is_none_or(|id| chat.space_id.as_ref() == Some(id))
            })
            .map(|chat| {
                let session = sessions.iter().find(|session| session.chat_id == chat.id);
                cross_task_summary(&chat, session)
            })
            .filter(|task| {
                request
                    .text
                    .as_ref()
                    .is_none_or(|text| task.title.to_lowercase().contains(&text.to_lowercase()))
            })
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        let after = request
            .cursor
            .as_deref()
            .map(|cursor| {
                serde_json::from_str::<(i64, String)>(cursor).map_err(|_| CrossTaskError::Input)
            })
            .transpose()?;
        if let Some((time, id)) = after {
            tasks.retain(|task| {
                task.updated_at_ms < time || task.updated_at_ms == time && task.task_id > id
            });
        }
        let limit = usize::from(request.limit.unwrap_or(10));
        let has_more = tasks.len() > limit;
        tasks.truncate(limit);
        let next_cursor = if has_more {
            tasks
                .last()
                .map(|task| serde_json::to_string(&(task.updated_at_ms, &task.task_id)))
                .transpose()
                .map_err(|_| CrossTaskError::InvalidBackendResult)?
        } else {
            None
        };
        Ok(zeron_proto::CrossTaskListPage {
            tasks,
            next_cursor,
            has_more,
        })
    }

    async fn read(
        &self,
        request: zeron_proto::CrossTaskReadRequest,
    ) -> Result<zeron_proto::CrossTaskReadPage, CrossTaskError> {
        let task = CrossTaskBackend::task(self, &request.task_id)
            .await?
            .ok_or(CrossTaskError::TargetScope)?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|cursor| {
                serde_json::from_str::<HistoryCursor>(cursor).map_err(|_| CrossTaskError::Input)
            })
            .transpose()?;
        let (events, next_cursor, has_more) = self
            .store
            .read_activity_task_page(
                &request.task_id,
                cursor.as_ref(),
                usize::from(request.limit.unwrap_or(10)),
            )
            .map_err(|_| CrossTaskError::Backend("redacted history read failed".into()))?;
        let turns = events
            .into_iter()
            .map(|event| zeron_proto::CrossTaskTurnSummary {
                turn_id: event.turn_id,
                status: serde_json::to_value(event.outcome)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into()),
                summary: event.summary,
            })
            .collect();
        let next_cursor = next_cursor
            .map(|cursor| serde_json::to_string(&cursor))
            .transpose()
            .map_err(|_| CrossTaskError::InvalidBackendResult)?;
        Ok(zeron_proto::CrossTaskReadPage {
            task,
            turns,
            next_cursor,
            has_more,
            content_untrusted: true,
        })
    }

    async fn enqueue_queue(
        &self,
        request: zeron_proto::CrossTaskSendRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<zeron_proto::CrossTaskSendReceipt, CrossTaskError> {
        if !authority.explicit_user_request || !authority.granted_scopes.contains("threads.send") {
            return Err(CrossTaskError::UserRequest);
        }
        if request.mode != zeron_proto::CrossTaskSendMode::Queue {
            return Err(CrossTaskError::Unsupported);
        }
        let task = CrossTaskBackend::task(self, &request.task_id)
            .await?
            .ok_or(CrossTaskError::TargetScope)?;
        if task.environment_id != authority.local_environment_id
            || !task
                .project_id
                .as_ref()
                .is_some_and(|id| authority.visible_project_ids.contains(id))
                && !(task.project_id.is_none() && authority.allow_projectless)
        {
            return Err(CrossTaskError::TargetScope);
        }
        let command = cross_task_queue_command(&request, authority);
        let receipt = self
            .execute_task_command(command, true)
            .await
            .map_err(|_| CrossTaskError::Backend("durable queue command failed".into()))?;
        if receipt.status != CommandReceiptStatus::Applied {
            return Err(CrossTaskError::CommandOutcome {
                status: receipt.status,
                reason: receipt.reason.as_ref().map(|reason| reason.code),
            });
        }
        let sequence = receipt
            .event_sequence
            .ok_or(CrossTaskError::InvalidBackendResult)?;
        Ok(zeron_proto::CrossTaskSendReceipt {
            task_id: request.task_id,
            queued_message_id: request.idempotency_key,
            activity_cursor: format!("command:{sequence}"),
            command: receipt,
            delivery_state: "queued".into(),
        })
    }

    async fn wait_event(
        &self,
        request: zeron_proto::CrossTaskWaitRequest,
    ) -> Result<Option<zeron_proto::CrossTaskWaitEvent>, CrossTaskError> {
        let mut sessions = self.workspace.watch_session_rows();
        let mut chats = self.workspace.watch_chats();
        loop {
            for target in &request.targets {
                let Some(task) = CrossTaskBackend::task(self, &target.task_id).await? else {
                    continue;
                };
                let row = sessions
                    .borrow()
                    .iter()
                    .find(|row| row.chat_id == target.task_id)
                    .cloned();
                let needs_attention = matches!(
                    task.status.as_str(),
                    "awaitingApproval" | "awaitingInput" | "errored"
                ) || row.as_ref().is_some_and(|row| {
                    matches!(
                        row.status,
                        zeron_proto::SessionStatus::AwaitingApproval
                            | zeron_proto::SessionStatus::AwaitingInput
                            | zeron_proto::SessionStatus::Errored
                    )
                });
                let completed = task.status == "completed"
                    || row.as_ref().is_some_and(|row| {
                        row.status == zeron_proto::SessionStatus::Idle
                            && row.last_completed_turn.is_some()
                    });
                if !needs_attention && !completed {
                    continue;
                }
                let cursor_material = format!(
                    "{}:{}:{}:{}",
                    task.task_id,
                    row.as_ref()
                        .map(|row| row.updated_at.timestamp_millis())
                        .unwrap_or(task.updated_at_ms),
                    task.status,
                    row.as_ref()
                        .and_then(|row| row.last_completed_turn.as_deref())
                        .unwrap_or("")
                );
                let cursor = format!("state:{:x}", Sha256::digest(cursor_material.as_bytes()));
                if target.after_cursor.as_deref() != Some(cursor.as_str()) {
                    return Ok(Some(zeron_proto::CrossTaskWaitEvent {
                        task,
                        cursor,
                        needs_attention,
                        completed,
                    }));
                }
            }
            tokio::select! {
                changed = sessions.changed() => if changed.is_err() { return Err(CrossTaskError::Unavailable); },
                changed = chats.changed() => if changed.is_err() { return Err(CrossTaskError::Unavailable); },
            }
        }
    }

    async fn control(
        &self,
        request: zeron_proto::CrossTaskControlRequest,
        authority: &CrossTaskAuthority,
        grant: &CrossTaskControlGrant,
    ) -> Result<zeron_proto::CrossTaskControlReceipt, CrossTaskError> {
        use zeron_proto::CrossTaskControlAction as Action;
        if !authority.explicit_user_request
            || !authority.granted_scopes.contains("threads.manage")
            || grant.caller_task_id != authority.caller_task_id
            || grant.request != request
            || grant.expires_at_ms <= crate::now_ms()
            || grant.expires_at_ms > crate::now_ms().saturating_add(60_000)
        {
            return Err(CrossTaskError::UserRequest);
        }
        let caller = CrossTaskBackend::task(self, &authority.caller_task_id)
            .await?
            .ok_or(CrossTaskError::TargetScope)?;
        if caller.environment_id != self.engine_info.device_id
            || caller.project_id.as_deref() != Some(request.project_id.as_str())
            || request.environment_id != self.engine_info.device_id
            || !authority.visible_project_ids.contains(&request.project_id)
        {
            return Err(CrossTaskError::TargetScope);
        }
        match request.action {
            Action::Create => {
                if self.task_identity.view(&request.task_id).is_some()
                    || self
                        .workspace
                        .chat(&request.task_id)
                        .map_err(|_| CrossTaskError::Backend("task lookup failed".into()))?
                        .is_some()
                {
                    // Let the durable command ledger handle an exact retry;
                    // a different command cannot create over this task id.
                    let prior = self
                        .doc_host
                        .task_command_receipt(&request.command_id)
                        .map_err(|_| CrossTaskError::Backend("command lookup failed".into()))?;
                    if !prior.is_some_and(|receipt| {
                        receipt.task_id == request.task_id && receipt.intent == "createChat"
                    }) {
                        return Err(CrossTaskError::TargetScope);
                    }
                }
                let destination = self
                    .workspace
                    .space(&request.project_id)
                    .map_err(|_| CrossTaskError::Backend("project lookup failed".into()))?
                    .ok_or(CrossTaskError::TargetScope)?;
                if destination.device_id != self.engine_info.device_id {
                    return Err(CrossTaskError::TargetScope);
                }
            }
            Action::Interrupt | Action::Archive | Action::Unarchive | Action::Move => {
                let target = CrossTaskBackend::task(self, &request.task_id)
                    .await?
                    .ok_or(CrossTaskError::TargetScope)?;
                if target.project_id.as_deref() != Some(request.project_id.as_str())
                    || target.environment_id != self.engine_info.device_id
                {
                    return Err(CrossTaskError::TargetScope);
                }
                if matches!(request.action, Action::Archive | Action::Unarchive)
                    && self
                        .task_identity
                        .has_native_codex_binding(&request.task_id)
                {
                    // Native Codex archive changes provider-owned state; it is
                    // not proven by the synthetic local control fixture.
                    return Err(CrossTaskError::Unsupported);
                }
                if request.action == Action::Move {
                    let destination_id = request
                        .destination_project_id
                        .as_deref()
                        .ok_or(CrossTaskError::Input)?;
                    if !authority.visible_project_ids.contains(destination_id) {
                        return Err(CrossTaskError::TargetScope);
                    }
                    let destination = self
                        .workspace
                        .space(destination_id)
                        .map_err(|_| CrossTaskError::Backend("destination lookup failed".into()))?
                        .ok_or(CrossTaskError::TargetScope)?;
                    if destination.device_id != self.engine_info.device_id {
                        return Err(CrossTaskError::TargetScope);
                    }
                }
            }
        }
        let command = cross_task_control_command(&request, authority);
        let receipt = self
            .execute_task_command(command, false)
            .await
            .map_err(|_| {
                CrossTaskError::Backend(
                    "command outcome is uncertain; retry only the same command id".into(),
                )
            })?;
        let observed_status = CrossTaskBackend::task(self, &request.task_id)
            .await?
            .map(|task| task.status)
            .unwrap_or_else(|| "missing".into());
        let settled =
            receipt.status.is_terminal() && !self.sessions.turn_in_flight(&request.task_id);
        Ok(zeron_proto::CrossTaskControlReceipt {
            task_id: request.task_id,
            command: receipt,
            observed_status,
            settled,
        })
    }
}

async fn backup_rpc_reply<T, F>(operation: F) -> Result<RpcReply, RpcError>
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> Result<T, BackupControlError> + Send + 'static,
{
    let value = tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| RpcError::Failed(format!("backup worker stopped: {error}")))?
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    RpcReply::value(&value)
}

#[async_trait]
impl RpcService for EngineRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if local_only_credential_method(method)
            && params
                .get("targetDeviceId")
                .and_then(|value| value.as_str())
                .is_some_and(|target| target != self.doc_host.device_id())
        {
            return Err(RpcError::Failed(
                "Remote credential actions are unavailable until paired end-to-end transport is implemented."
                    .into(),
            ));
        }
        // Device-addressed routing: forward calls that target another device over its
        // relay. The target compares the id to its own, so forwards cannot loop.
        if forwardable(method)
            && let Some(target) = params.get("targetDeviceId").and_then(|v| v.as_str())
            && target != self.doc_host.device_id()
        {
            let target = target.to_string();
            return self.forward(&target, method, params).await;
        }
        if AuthRpc::handles(method) {
            return AuthRpc::new(self.auth()?.clone())
                .handle(method, params)
                .await;
        }
        match method {
            methods::ENGINE_INFO => RpcReply::value(&self.engine_info),
            methods::ENGINE_READY => RpcReply::value(&serde_json::json!({ "ready": true })),
            methods::GET_BACKUP_STATUS => {
                let Some(control) = self.backup_control.clone() else {
                    return RpcReply::value(&BackupStatus {
                        availability: BackupAvailability::Unavailable,
                        reason: Some("Live-profile backup is unavailable; configure a separate copy-isolated source with an owner-held recovery key.".into()),
                        source_root: None,
                        destination_dir: None,
                        automatic_enabled: false,
                        key_configured: false,
                        last_success: None,
                        retention_candidates: 0,
                        staged_restore: None,
                        restore_activation_available: false,
                        restore_reason: "Live restore is unavailable until staged verification and post-start rollback are complete.".into(),
                    });
                };
                backup_rpc_reply(move || control.status()).await
            }
            methods::CONFIGURE_BACKUP => {
                let request: BackupConfigureRequest = parse_params(params)?;
                let control = self.backup_control()?;
                backup_rpc_reply(move || control.configure(request)).await
            }
            methods::CONFIGURE_BACKUP_KEY => {
                let request: BackupKeyConfigureRequest = parse_params(params)?;
                let control = self.backup_control()?;
                backup_rpc_reply(move || {
                    control.configure_recovery_key_hex(request.recovery_key_hex)
                })
                .await
            }
            methods::BACKUP_NOW => {
                let control = self.backup_control()?;
                backup_rpc_reply(move || control.manual_backup()).await
            }
            methods::EXPORT_BACKUP => {
                let request: BackupArchiveRequest = parse_params(params)?;
                let control = self.backup_control()?;
                backup_rpc_reply(move || control.export(request)).await
            }
            methods::INSPECT_BACKUP => {
                let request: BackupArchiveRequest = parse_params(params)?;
                let control = self.backup_control()?;
                backup_rpc_reply(move || control.inspect(request)).await
            }
            methods::STAGE_BACKUP_RESTORE => {
                let request: BackupArchiveRequest = parse_params(params)?;
                let control = self.backup_control()?;
                backup_rpc_reply(move || control.stage_restore(request)).await
            }
            methods::GET_ATTENTION => {
                let request: AttentionReadRequest = parse_params(params)?;
                self.ensure_local_attention_target(&request.target_device_id)?;
                RpcReply::value(&self.reconcile_attention()?)
            }
            methods::CLAIM_ATTENTION_TRANSITIONS => {
                let request: AttentionTransitionClaim = parse_params(params)?;
                self.ensure_local_attention_target(
                    request.target_device_id.as_deref().unwrap_or(""),
                )?;
                self.reconcile_attention()?;
                let transitions = self
                    .store
                    .claim_attention_transitions(&request.consumer_id, request.deliver_existing)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&AttentionTransitionBatch {
                    through_sequence: transitions.last().map_or(0, |row| row.sequence),
                    transitions,
                })
            }
            methods::MUTATE_ATTENTION => {
                let request: AttentionMutationRequest = parse_params(params)?;
                self.ensure_local_attention_target(
                    request.target_device_id.as_deref().unwrap_or(""),
                )?;
                if request.command_id.is_empty() || request.command_id.len() > 128 {
                    return Err(RpcError::BadParams("invalid Activity commandId".into()));
                }
                self.reconcile_attention()?;
                let result = self
                    .store
                    .mutate_attention(
                        &request.command_id,
                        request.expected_revision,
                        &request.mutation,
                    )
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let snapshot = self.reconcile_attention()?;
                RpcReply::value(&AttentionMutationResult {
                    command_id: request.command_id,
                    applied: !result.duplicate,
                    duplicate: result.duplicate,
                    // Receipt revision is stable for an idempotent replay even
                    // when the accompanying fresh snapshot has advanced.
                    revision: result.revision,
                    snapshot,
                })
            }
            methods::SEARCH_SESSION_HISTORY => {
                let request: HistorySearchRequest = parse_params(params)?;
                let page = self
                    .store
                    .search_activity_events(&request)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&page)
            }
            methods::GET_USAGE_SNAPSHOT => {
                let request: UsageRequest = parse_params(params)?;
                let mut snapshot =
                    tokio::task::spawn_blocking(move || crate::usage::snapshot(request))
                        .await
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                for source in &mut snapshot.summary.sources {
                    source.environment_id = self.engine_info.device_id.clone();
                }
                RpcReply::value(&snapshot)
            }
            methods::REBUILD_SESSION_HISTORY => {
                let request: HistoryRebuildRequest = parse_params(params)?;
                let requested = request.task_ids.into_iter().collect::<HashSet<_>>();
                let chats = self
                    .workspace
                    .read_chats()
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .into_iter()
                    .filter(|chat| requested.is_empty() || requested.contains(&chat.id))
                    .collect::<Vec<_>>();
                let _activity_guard = self.store.activity_index_guard();
                let mut rebuilt = Vec::with_capacity(chats.len());
                for chat in &chats {
                    let handle = self
                        .doc_host
                        .open(&chat.id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    let entries = handle
                        .doc()
                        .read_entries()
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    let policy = chat
                        .space_id
                        .as_deref()
                        .map(|project_id| self.store.activity_project_policy(project_id))
                        .transpose()
                        .map_err(|error| RpcError::Failed(error.to_string()))?
                        .flatten();
                    let (receipts, receipts_truncated) =
                        task_command_history(&self.store, &chat.id)
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    let identity = self.task_identity.view(&chat.id);
                    let run_metadata = self.sessions.activity_run_metadata(&chat.id);
                    let events = crate::activity_history::build_activity_envelopes(
                        &self.engine_info.device_id,
                        chat,
                        identity.as_ref(),
                        &entries,
                        &receipts,
                        receipts_truncated,
                        &run_metadata,
                        policy.as_ref(),
                    );
                    rebuilt.push((chat.id.clone(), events));
                }
                let indexed_events = rebuilt.iter().map(|(_, events)| events.len() as u64).sum();
                if requested.is_empty() {
                    self.store
                        .rebuild_activity_events(&rebuilt)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                } else {
                    for (task_id, events) in &rebuilt {
                        self.store
                            .replace_activity_events(task_id, events)
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    }
                }
                RpcReply::value(&HistoryRebuildResult {
                    indexed_events,
                    indexed_tasks: rebuilt.len() as u64,
                    completed_at_ms: chrono::Utc::now().timestamp_millis(),
                })
            }
            methods::GET_SESSION_HISTORY_POLICY => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    project_id: String,
                }
                let p: P = parse_params(params)?;
                let policy = self
                    .store
                    .activity_project_policy(&p.project_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .unwrap_or(HistoryProjectPolicy {
                        project_id: p.project_id,
                        excluded_path_prefixes: Vec::new(),
                        excluded_terms: Vec::new(),
                        index_terminal_output: false,
                        index_browser_fields: false,
                        index_accessibility_text: false,
                        index_screenshot_text: false,
                    });
                RpcReply::value(&policy)
            }
            methods::SET_SESSION_HISTORY_POLICY => {
                let policy: HistoryProjectPolicy = parse_params(params)?;
                if policy.project_id.trim().is_empty() {
                    return Err(RpcError::Failed("projectId is required".into()));
                }
                if policy.index_terminal_output
                    || policy.index_browser_fields
                    || policy.index_accessibility_text
                    || policy.index_screenshot_text
                {
                    return Err(RpcError::Failed(
                        "Sensitive terminal, browser-field, Accessibility, and screenshot text indexing is unsupported."
                            .into(),
                    ));
                }
                self.store
                    .save_activity_project_policy(&policy)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                // Reconcile affected tasks immediately so old derived rows do
                // not survive a stricter disclosure policy.
                for chat in self
                    .workspace
                    .read_chats()
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .into_iter()
                    .filter(|chat| chat.space_id.as_deref() == Some(policy.project_id.as_str()))
                {
                    let handle = self
                        .doc_host
                        .open(&chat.id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    index_activity_task(
                        &self.store,
                        &self.task_identity,
                        &self.sessions,
                        &self.engine_info.device_id,
                        &chat,
                        handle
                            .doc()
                            .read_entries()
                            .map_err(|error| RpcError::Failed(error.to_string()))?,
                    )
                    .map_err(RpcError::Failed)?;
                }
                RpcReply::value(&policy)
            }
            methods::GET_TASK_IDENTITY => {
                let p: TaskIdentityParams = parse_params(params)?;
                RpcReply::value(&self.task_identity.view(&p.task_id))
            }
            methods::GET_RUNTIME_CONTRACT => {
                let p: TaskIdentityParams = parse_params(params)?;
                let chat = self
                    .workspace
                    .chat(&p.task_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                if chat.is_none() {
                    return Err(RpcError::Failed("task not found".into()));
                }
                if !self.workspace.is_host(&p.task_id) {
                    return Err(RpcError::Failed(
                        "runtime diagnostics require the task's owning device; pass targetDeviceId"
                            .into(),
                    ));
                }
                let binding = self
                    .task_identity
                    .view(&p.task_id)
                    .and_then(|view| view.current_binding);
                let provider = chat
                    .as_ref()
                    .and_then(|chat| chat.config.as_ref().map(|config| config.harness))
                    .or_else(|| binding.as_ref().map(|binding| binding.provider_id));
                let provider_delivery = provider
                    .and_then(|provider| self.registry.resolve(provider).ok())
                    .map(|adapter| adapter.luna_runtime_delivery())
                    .unwrap_or(zeron_proto::RuntimeDelivery::Unsupported);
                let session_id = binding
                    .map(|binding| binding.provider_task_id)
                    .filter(|id| !id.is_empty())
                    .or_else(|| {
                        chat.as_ref()
                            .and_then(|chat| chat.harness_session_id.clone())
                    });
                let mut receipt = session_id
                    .as_deref()
                    .map(|id| self.sessions.runtime_contract_diagnostics(&p.task_id, id))
                    .transpose()
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .flatten();
                if let (Some(receipt), Some(request)) = (
                    receipt.as_mut(),
                    self.doc_host.request_from_chat_row(&p.task_id, ""),
                ) {
                    let context = self
                        .sessions
                        .runtime_context_for(
                            &p.task_id,
                            request.harness.unwrap_or(HarnessId::Mock),
                            &request,
                        )
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    let (user_instructions, workspace_instructions) = self
                        .sessions
                        .integration_settings()
                        .map(|settings| settings.contributions(context.project_id.as_deref()))
                        .unwrap_or_default();
                    let (cross_task_tools, cross_task_omissions) = self
                        .sessions
                        .cross_task_catalog(&context, provider_delivery);
                    let compiled =
                        crate::runtime_contract::compile(crate::runtime_contract::CompileInput {
                            context,
                            provider_delivery,
                            user_instructions,
                            workspace_instructions,
                            resume: true,
                            acknowledged_contract: Some(receipt.clone()),
                            cross_task_tools,
                            cross_task_omissions,
                        })
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    receipt.restart_required =
                        compiled.restart_required || compiled.must_reconstruct;
                }
                let has_receipt = receipt.is_some();
                if let Some(receipt) = receipt.as_mut() {
                    // Provider-native session identifiers remain an engine
                    // detail. The app-facing readback uses the LUNA task id.
                    receipt.provider_session_id.clear();
                }
                RpcReply::value(&zeron_proto::RuntimeContractView {
                    task_id: p.task_id,
                    provider,
                    provider_delivery,
                    effective: receipt,
                    unavailable_reason: match provider_delivery {
                        zeron_proto::RuntimeDelivery::Unsupported => Some(
                            "This provider has no verified LUNA runtime instruction or tool delivery path.".into()
                        ),
                        zeron_proto::RuntimeDelivery::ClaudeInstructionsOnly => Some(
                            "Claude instructions are supported; LUNA tools are unavailable until its scoped MCP transport is verified.".into()
                        ),
                        _ if !has_receipt => Some(
                            "No provider session has acknowledged a LUNA runtime contract yet.".into()
                        ),
                        _ => None,
                    },
                })
            }
            methods::GET_INSTRUCTION_SETTINGS => {
                let p: InstructionSettingsParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                if let Some(id) = p.project_id.as_deref() {
                    let space = self
                        .workspace
                        .space(id)
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                        .ok_or_else(|| RpcError::BadParams("unknown project id".into()))?;
                    if space.device_id != self.engine_info.device_id {
                        return Err(RpcError::Failed(
                            "project instructions require the owning device".into(),
                        ));
                    }
                }
                let settings = self.sessions.integration_settings().ok_or_else(|| {
                    RpcError::Failed("LUNA instruction settings are unavailable".into())
                })?;
                RpcReply::value(&settings.view(p.project_id.as_deref()))
            }
            methods::SET_INSTRUCTION_SETTINGS => {
                let p: SaveInstructionSettingsParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                if let Some(id) = p.project_id.as_deref() {
                    let space = self
                        .workspace
                        .space(id)
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                        .ok_or_else(|| RpcError::BadParams("unknown project id".into()))?;
                    if space.device_id != self.engine_info.device_id {
                        return Err(RpcError::Failed(
                            "project instructions require the owning device".into(),
                        ));
                    }
                }
                let settings = self.sessions.integration_settings().ok_or_else(|| {
                    RpcError::Failed("LUNA instruction settings are unavailable".into())
                })?;
                let saved = settings
                    .save(p.project_id.as_deref(), &p.text, p.expected_revision)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&saved)
            }
            methods::GET_INTEGRATION_INVENTORY => {
                let p: IntegrationInventoryParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let project_root = if let Some(id) = p.project_id.as_deref() {
                    let space = self
                        .workspace
                        .space(id)
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                        .ok_or_else(|| RpcError::BadParams("unknown project id".into()))?;
                    if space.device_id != self.engine_info.device_id {
                        return Err(RpcError::Failed(
                            "integration inventory requires the owning device".into(),
                        ));
                    }
                    Some(space.path)
                } else {
                    None
                };
                let view = if p.provider == HarnessId::Codex {
                    let home = self
                        .agent_accounts
                        .codex_home_for_inventory()
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    crate::integration_settings::codex_inventory(
                        &home,
                        project_root.as_deref().map(std::path::Path::new),
                        &self.engine_info.device_id,
                    )
                } else {
                    crate::integration_settings::unavailable_inventory(
                        p.provider,
                        &self.engine_info.device_id,
                    )
                };
                RpcReply::value(&view)
            }
            methods::GET_HISTORY_CAPABILITY => {
                let p: TaskIdentityParams = parse_params(params)?;
                let provider = self
                    .task_identity
                    .view(&p.task_id)
                    .and_then(|view| view.current_binding)
                    .map(|binding| binding.provider_id);
                let capability = provider
                    .map(capability_for)
                    .unwrap_or(ProviderForkCapability::None);
                RpcReply::value(&HistoryCapabilityView {
                    task_id: p.task_id,
                    provider,
                    capability,
                    unavailable_reason: provider
                        .and_then(unavailable_reason)
                        .map(str::to_owned)
                        .or_else(|| {
                            (provider.is_none()).then(|| {
                                "This task has no materialized provider history to fork.".into()
                            })
                        }),
                })
            }
            methods::LIST_HISTORY_RECOVERIES => {
                let p: TaskIdentityParams = parse_params(params)?;
                let recoveries =
                    crate::history_operations::list_recoveries(&self.store, &p.task_id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&recoveries)
            }
            methods::REFRESH_TASK_BINDING => {
                let p: TaskIdentityParams = parse_params(params)?;
                let view = self
                    .task_identity
                    .reconcile_task(&p.task_id, self.sessions.has_live_run(&p.task_id))
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&view)
            }
            methods::PREVIEW_PROVIDER_TASK_IMPORT => {
                let p: PreviewProviderTaskImportParams = parse_params(params)?;
                let preview = self
                    .task_identity
                    .preview_import(
                        p.provider_id,
                        &p.provider_task_id,
                        p.ownership,
                        p.space_id.as_deref(),
                        p.device_id.as_deref(),
                    )
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&preview)
            }
            methods::ADOPT_PROVIDER_TASK => {
                let p: AdoptProviderTaskParams = parse_params(params)?;
                let config = p.config.or_else(|| {
                    Some(ChatConfig {
                        harness: HarnessId::Codex,
                        provider_account_id: None,
                        model: None,
                        reasoning: None,
                        model_options: serde_json::Map::new(),
                        sandbox: zeron_proto::SandboxLevel::WorkspaceWrite,
                        permission_mode: zeron_proto::PermissionMode::Ask,
                        effective_permission_mode: zeron_proto::PermissionMode::Ask,
                        permission_ceiling_source: None,
                        permission_degradation_reason: None,
                        permission_audit: Vec::new(),
                    })
                });
                let view = self
                    .task_identity
                    .adopt_preview(&p.preview_id, config)
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&view)
            }
            methods::EXECUTE_TASK_COMMAND => {
                let command: TaskCommandParams = parse_params(params)?;
                let receipt = self.execute_task_command(command, false).await?;
                RpcReply::value(&receipt)
            }
            methods::GET_TASK_COMMAND_RECEIPT => {
                let p: TaskCommandReceiptParams = parse_params(params)?;
                RpcReply::value(
                    &self
                        .doc_host
                        .task_command_receipt(&p.command_id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?,
                )
            }
            methods::GET_COMPOSER_DRAFT => {
                let p: ComposerDraftReadParams = parse_params(params)?;
                let read = self.read_composer_draft(p.task_id.as_deref(), p.draft_id.as_deref())?;
                RpcReply::value(&read)
            }
            methods::SAVE_COMPOSER_DRAFT => {
                let p: ComposerDraftSaveParams = parse_params(params)?;
                self.validate_composer_draft(&p.draft)?;
                let outcome = self
                    .store
                    .save_composer_draft(&self.engine_info.device_id, &p.draft)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&outcome)
            }
            methods::BEGIN_DRAFT_SUBMISSION => {
                let p: BeginDraftSubmissionParams = parse_params(params)?;
                self.validate_composer_draft(&p.snapshot)?;
                self.store
                    .begin_draft_submission(
                        &self.engine_info.device_id,
                        &p.command_id,
                        p.task_id.as_deref(),
                        &p.snapshot,
                    )
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&serde_json::json!({ "stored": true }))
            }
            methods::SETTLE_DRAFT_SUBMISSION => {
                let p: SettleDraftSubmissionParams = parse_params(params)?;
                match p.outcome {
                    DraftSubmissionOutcome::Pending => {}
                    DraftSubmissionOutcome::Recoverable => self
                        .store
                        .mark_draft_submission_recoverable(
                            &self.engine_info.device_id,
                            &p.command_id,
                        )
                        .map_err(|error| RpcError::Failed(error.to_string()))?,
                    DraftSubmissionOutcome::Applied | DraftSubmissionOutcome::Recovered => {
                        self.store
                            .accept_draft_submission(
                                &self.engine_info.device_id,
                                &p.command_id,
                                p.queue_id.as_deref(),
                            )
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    }
                }
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::GET_TASK_COMMAND_SNAPSHOT => {
                let p: TaskCommandSnapshotParams = parse_params(params)?;
                let mut read = match p.after_task_sequence {
                    Some(cursor) => {
                        self.doc_host
                            .read_task_commands(&p.task_id, cursor, p.limit.unwrap_or(50))
                    }
                    None => self.doc_host.task_command_snapshot(&p.task_id),
                }
                .map_err(|error| RpcError::Failed(error.to_string()))?;
                let current = self.current_task_command_snapshot(&p.task_id)?;
                read.snapshot.binding_id = current.binding_id;
                read.snapshot.binding_generation = current.binding_generation;
                read.snapshot.active_turn_id = current.active_turn_id;
                RpcReply::value(&read)
            }
            methods::WATCH_TASK_COMMANDS => {
                let p: TaskCommandSnapshotParams = parse_params(params)?;
                let mut read = match p.after_task_sequence {
                    Some(cursor) => {
                        self.doc_host
                            .read_task_commands(&p.task_id, cursor, p.limit.unwrap_or(50))
                    }
                    None => self.doc_host.task_command_snapshot(&p.task_id),
                }
                .map_err(|error| RpcError::Failed(error.to_string()))?;
                let current = self.current_task_command_snapshot(&p.task_id)?;
                read.snapshot.binding_id = current.binding_id;
                read.snapshot.binding_generation = current.binding_generation;
                read.snapshot.active_turn_id = current.active_turn_id;
                let rx = self.doc_host.subscribe_task_receipts();
                let task_id = p.task_id;
                Ok(RpcReply::Stream(
                    futures::stream::unfold((Some(read), rx), move |(initial, mut rx)| {
                        let task_id = task_id.clone();
                        async move {
                            if let Some(read) = initial {
                                return Some((
                                    serde_json::json!({ "kind": "snapshot", "read": read }),
                                    (None, rx),
                                ));
                            }
                            loop {
                                match rx.recv().await {
                                    Ok(receipt) if receipt.task_id == task_id => {
                                        let value = serde_json::json!({
                                            "kind": "receipt",
                                            "receipt": receipt,
                                        });
                                        return Some((value, (None, rx)));
                                    }
                                    Ok(_)
                                    | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        return None;
                                    }
                                }
                            }
                        }
                    })
                    .boxed(),
                ))
            }
            methods::LIST_HARNESSES => RpcReply::value(&self.registry.descriptors()),
            methods::GET_TITLE_SETTINGS => RpcReply::value(&self.registry.title_settings()),
            methods::SET_TITLE_SETTINGS => {
                let p: crate::registry::TitleSettings = parse_params(params)?;
                self.registry
                    .set_title_settings(p)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&self.registry.title_settings())
            }
            methods::SET_HARNESS_ENABLED => {
                let p: SetHarnessEnabledParams = parse_params(params)?;
                update_harness_enabled(
                    &self.registry,
                    p.harness,
                    p.enabled,
                    zeron_harness::AcpHarness::antigravity().sign_out(),
                )
                .await?;
                // Fresh catalog in the reply: the page repaints from it in one
                // round trip, and a refused/raced toggle self-corrects.
                RpcReply::value(&self.registry.descriptors())
            }
            methods::LIST_MODELS => {
                let p: ListModelsParams = parse_params(params)?;
                if p.harness == HarnessId::Codex {
                    let execution = self
                        .agent_accounts
                        .resolve_execution(HarnessId::Codex, p.account_id.as_deref())
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    let home = execution.codex_home.ok_or_else(|| {
                        RpcError::Failed("Codex account did not resolve an execution home".into())
                    })?;
                    let models = zeron_harness::codex::CodexHarness::new()
                        .models_for_home(&home, execution.codex_file_auth)
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    return RpcReply::value(&models);
                }
                let harness = self
                    .registry
                    .resolve(p.harness)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let models = harness
                    .models()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&models)
            }
            methods::LIST_COMMANDS => {
                // Same shape as ListModels: forces a lazy resolve, then the
                // harness's own (cached) discovery — ACP agents advertise
                // availableCommands, claude answers the initialize control
                // request, codex lists skills; only harnesses whose wire has
                // no listing (cursor, mock) fall through to the trait's
                // empty default.
                let p: ListModelsParams = parse_params(params)?;
                let harness = self
                    .registry
                    .resolve(p.harness)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let commands = harness
                    .commands()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&commands)
            }
            methods::QUEUE_COMMAND => {
                let p: QueueCommandParams = parse_params(params)?;
                let command = legacy_task_command(
                    p.chat_id,
                    TaskCommandIntent::Session {
                        command: p.command,
                        transfers: p.transfers,
                    },
                );
                let receipt = self.execute_task_command(command, true).await?;
                RpcReply::value(&serde_json::json!({
                    "commandId": receipt.command_id,
                    "receipt": receipt,
                }))
            }
            methods::RETRY_DELIVERY => {
                let p: ChatParams = parse_params(params)?;
                self.doc_host
                    .retry_delivery(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({}))
            }
            methods::RELAY_COMMAND => {
                let p: RelayCommandParams = parse_params(params)?;
                let outcome = self
                    .doc_host
                    .ingest_relayed_command(&p.chat_id, p.entry)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "outcome": outcome }))
            }
            methods::WATCH_DOC_MESSAGES => {
                let p: ChatParams = parse_params(params)?;
                if self
                    .task_identity
                    .view(&p.chat_id)
                    .and_then(|view| view.current_binding)
                    .is_some()
                {
                    let identity = self.task_identity.clone();
                    let task_id = p.chat_id.clone();
                    let luna_active = self.sessions.has_live_run(&task_id);
                    tokio::spawn(async move {
                        if let Err(error) = identity.reconcile_task(&task_id, luna_active).await {
                            tracing::warn!(task = %task_id, %error, "task-open reconciliation failed");
                        }
                    });
                }
                let handle = self
                    .doc_host
                    .open(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                Ok(RpcReply::Stream(doc_messages_stream(
                    handle.watch_messages(),
                    handle.doc_arc(),
                )))
            }
            methods::WATCH_QUEUE => {
                let p: ChatParams = parse_params(params)?;
                let handle = self
                    .doc_host
                    .open(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let rx = handle.watch_queue();
                let store = self.store.clone();
                let uploads = self.uploads.clone();
                let device_id = self.engine_info.device_id.clone();
                Ok(RpcReply::Stream(
                    futures::stream::unfold((rx, true), move |(mut rx, first)| {
                        let store = store.clone();
                        let uploads = uploads.clone();
                        let device_id = device_id.clone();
                        async move {
                            if !first {
                                rx.changed().await.ok()?;
                            }
                            let items = rx.borrow_and_update().clone();
                            let owners = items
                                .iter()
                                .map(|item| {
                                    let paths = item
                                        .attachments
                                        .iter()
                                        .filter_map(|path| uploads.owned_blob_path(path))
                                        .collect();
                                    (item.id.clone(), paths)
                                })
                                .collect::<Vec<_>>();
                            if let Err(error) =
                                store.replace_queue_attachment_owners(&device_id, owners)
                            {
                                tracing::warn!(%error, "queue attachment ownership refresh failed");
                            }
                            let value = serde_json::json!({ "items": items });
                            Some((value, (rx, false)))
                        }
                    })
                    .boxed(),
                ))
            }
            methods::QUEUE_MESSAGE => {
                let p: QueueMessageParams = parse_params(params)?;
                let id = p
                    .message_id
                    .or_else(|| p.command_id.clone())
                    .unwrap_or_else(crate::new_id);
                let mut command = legacy_task_command(
                    p.chat_id,
                    TaskCommandIntent::QueueMessage {
                        message_id: id.clone(),
                        text: p.text,
                        attachments: p.attachments,
                        hold_for_turn_end: p.hold_for_turn_end,
                        follow_up_behavior: p.follow_up_behavior,
                        execution: None,
                    },
                );
                if let Some(command_id) = p.command_id {
                    command.command_id = command_id;
                }
                let receipt = self.execute_task_command(command, true).await?;
                RpcReply::value(&serde_json::json!({ "id": id, "receipt": receipt }))
            }
            methods::UPDATE_QUEUED_MESSAGE => {
                let p: QueuedMessageParams = parse_params(params)?;
                let changed = self
                    .doc_host
                    .update_queued_message(&p.chat_id, &p.id, &p.text)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "changed": changed }))
            }
            methods::BEGIN_QUEUED_MESSAGE_EDIT => {
                let p: BeginQueuedMessageEditParams = parse_params(params)?;
                let outcome = self
                    .doc_host
                    .begin_queued_message_edit(
                        &p.chat_id,
                        &p.id,
                        &p.editor_device_id,
                        &p.editor_instance_id,
                    )
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(
                    &serde_json::to_value(outcome).map_err(|e| RpcError::Failed(e.to_string()))?,
                )
            }
            methods::RENEW_QUEUED_MESSAGE_EDIT => {
                let p: RenewQueuedMessageEditParams = parse_params(params)?;
                let outcome = self
                    .doc_host
                    .renew_queued_message_edit(&p.chat_id, &p.id, &p.lease_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(
                    &serde_json::to_value(outcome).map_err(|e| RpcError::Failed(e.to_string()))?,
                )
            }
            methods::FINISH_QUEUED_MESSAGE_EDIT => {
                let p: FinishQueuedMessageEditParams = parse_params(params)?;
                if matches!(p.action, FinishQueuedMessageEditAction::Commit) {
                    let text = p
                        .text
                        .clone()
                        .ok_or_else(|| RpcError::BadParams("commit requires text".into()))?;
                    let expected_text_hash = p.expected_text_hash.clone().ok_or_else(|| {
                        RpcError::BadParams("commit requires expectedTextHash".into())
                    })?;
                    let revision = match p.expected_queue_revision {
                        Some(revision) => revision,
                        None => self
                            .doc_host
                            .queued_message(&p.chat_id, &p.id)
                            .map_err(|error| RpcError::Failed(error.to_string()))?
                            .map(|item| item.revision)
                            .unwrap_or(0),
                    };
                    let mut command = legacy_task_command(
                        p.chat_id,
                        TaskCommandIntent::EditQueuedMessage {
                            message_id: p.id,
                            expected_queue_revision: revision,
                            lease_id: p.lease_id,
                            text,
                            expected_text_hash,
                            attachments: p.attachments.unwrap_or_default(),
                        },
                    );
                    if let Some(command_id) = p.command_id {
                        command.command_id = command_id;
                    }
                    let receipt = self.execute_task_command(command, true).await?;
                    let outcome = match receipt.reason.as_ref().map(|reason| reason.code) {
                        _ if receipt.status == CommandReceiptStatus::Applied => "committed",
                        Some(CommandReasonCode::QueueClaimed) => "claimed",
                        Some(CommandReasonCode::QueueMissing) => "missing",
                        _ => "conflict",
                    };
                    return RpcReply::value(&serde_json::json!({
                        "outcome": outcome,
                        "receipt": receipt,
                    }));
                }
                let action = match p.action {
                    FinishQueuedMessageEditAction::Commit => unreachable!(),
                    FinishQueuedMessageEditAction::Cancel => {
                        crate::doc_host::FinishQueueEditAction::Cancel
                    }
                    FinishQueuedMessageEditAction::Discard => {
                        crate::doc_host::FinishQueueEditAction::Discard
                    }
                    FinishQueuedMessageEditAction::ReleaseUnchanged => {
                        crate::doc_host::FinishQueueEditAction::ReleaseUnchanged
                    }
                };
                let outcome = self
                    .doc_host
                    .finish_queued_message_edit_with_attachments(
                        &p.chat_id,
                        &p.id,
                        &p.lease_id,
                        action,
                        p.text.as_deref(),
                        p.expected_text_hash.as_deref(),
                        p.attachments.as_deref(),
                    )
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(
                    &serde_json::to_value(outcome).map_err(|e| RpcError::Failed(e.to_string()))?,
                )
            }
            methods::MOVE_QUEUED_MESSAGE => {
                let p: QueuedMessageParams = parse_params(params)?;
                let changed = self
                    .doc_host
                    .move_queued_message(&p.chat_id, &p.id, p.to_index)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "changed": changed }))
            }
            methods::REMOVE_QUEUED_MESSAGE => {
                let p: QueuedMessageParams = parse_params(params)?;
                let existing = self
                    .doc_host
                    .queued_message(&p.chat_id, &p.id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let revision = existing.as_ref().map(|item| item.revision).unwrap_or(0);
                let receipt = self
                    .execute_task_command(
                        legacy_task_command(
                            p.chat_id,
                            TaskCommandIntent::RemoveQueuedMessage {
                                message_id: p.id,
                                expected_queue_revision: revision,
                            },
                        ),
                        true,
                    )
                    .await?;
                RpcReply::value(&serde_json::json!({
                    "removed": existing.is_some()
                        && receipt.status == CommandReceiptStatus::Applied,
                    "receipt": receipt,
                }))
            }
            methods::SEND_QUEUED_MESSAGE_NOW => {
                let p: QueuedMessageParams = parse_params(params)?;
                let receipt = self
                    .execute_task_command(
                        legacy_task_command(
                            p.chat_id,
                            TaskCommandIntent::SendQueuedNow {
                                message_id: p.id,
                                expected_queue_revision: None,
                            },
                        ),
                        true,
                    )
                    .await?;
                RpcReply::value(&serde_json::json!({
                    "sent": matches!(receipt.status, CommandReceiptStatus::Pending | CommandReceiptStatus::Applied),
                    "receipt": receipt,
                }))
            }
            methods::STEER_QUEUED_MESSAGE_NOW => {
                let p: QueuedMessageParams = parse_params(params)?;
                let receipt = self
                    .execute_task_command(
                        legacy_task_command(
                            p.chat_id,
                            TaskCommandIntent::SteerQueuedNow {
                                message_id: p.id,
                                expected_queue_revision: None,
                            },
                        ),
                        true,
                    )
                    .await?;
                RpcReply::value(&serde_json::json!({
                    "sent": matches!(receipt.status, CommandReceiptStatus::Pending | CommandReceiptStatus::Applied),
                    "receipt": receipt,
                }))
            }
            methods::PROBE_SYNC => {
                // Focus probes remain cheap. An explicit Retry may also allow
                // one fresh, shared auth attempt before its cooldown expires.
                if params.get("retry").and_then(serde_json::Value::as_bool) == Some(true)
                    && let Some(auth) = &self.auth
                {
                    auth.retry_refresh();
                }
                self.workspace.probe();
                self.doc_host.probe_open_chats();
                self.doc_host.probe_edge_reachability();
                RpcReply::value(&serde_json::json!({}))
            }
            methods::SYNC_STATUS => {
                fn room_json(s: &zeron_sync::RoomStatsSnapshot) -> serde_json::Value {
                    serde_json::json!({
                        "connected": s.connected,
                        "synced": s.synced,
                        "lastPushedMs": s.last_pushed_ms,
                        "lastAckMs": s.last_ack_ms,
                        "rejoins": s.rejoins,
                        "probes": s.probes,
                        "fullResyncs": s.full_resyncs,
                        "disconnects": s.disconnects,
                        "rejected": s.rejected,
                    })
                }
                fn chat2_json(s: &zeron_sync::ChatStatsSnapshot) -> serde_json::Value {
                    serde_json::json!({
                        "connected": s.connected,
                        "cursor": s.cursor,
                        "headSeq": s.head_seq,
                        "seqFloor": s.seq_floor,
                        "checkpointSeq": s.checkpoint_seq,
                        "checkpointSize": s.checkpoint_size,
                        "rowCount": s.row_count,
                        "rowBytes": s.row_bytes,
                        "pendingPushes": s.pending_pushes,
                        "rejoins": s.rejoins,
                        "disconnects": s.disconnects,
                        "rejected": s.rejected,
                        "serverResets": s.server_resets,
                    })
                }
                let workspace = self.workspace.sync_status();
                let chats: Vec<serde_json::Value> = self
                    .doc_host
                    .sync_statuses()
                    .iter()
                    .map(|(chat_id, room)| {
                        serde_json::json!({
                            "chatId": chat_id,
                            "room": room.as_ref().map(chat2_json),
                        })
                    })
                    .collect();
                RpcReply::value(&serde_json::json!({
                    "deviceId": self.doc_host.device_id(),
                    "nowMs": crate::now_ms(),
                    "workspace": workspace.as_ref().map(room_json),
                    "chats": chats,
                }))
            }
            methods::WATCH_CONNECTIVITY => Ok(RpcReply::Stream(watch_stream(
                self.doc_host.watch_connectivity(),
            ))),
            methods::WATCH_TRANSFERS => Ok(RpcReply::Stream(watch_stream(
                self.doc_host.watch_transfers(),
            ))),
            methods::WATCH_PREVIEWS => {
                let p: zeron_proto::WatchPreviewsParams = parse_params(params)?;
                if self
                    .workspace
                    .chat(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .is_none()
                {
                    return Err(RpcError::Failed("Project session not found".into()));
                }
                let previews = self
                    .previews
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("Preview discovery unavailable".into()))?;
                let catalog = previews.catalog().clone();
                let changes = catalog.subscribe();
                let chats = self.workspace.watch_chats();
                let workspace = self.workspace.clone();
                // This subscription stays on the viewing device. A remote chat
                // selects advertised services, but its URL uses our local proxy.
                let stream = futures::stream::unfold(
                    (changes, chats, true, workspace, catalog, p.chat_id),
                    |(mut changes, mut chats, first, workspace, catalog, chat_id)| async move {
                        if !first {
                            tokio::select! {
                                result = changes.changed() => { if result.is_err() { return None; } }
                                result = chats.changed() => { if result.is_err() { return None; } }
                            }
                        }
                        let mut snapshot = changes.borrow_and_update().clone();
                        chats.borrow_and_update();
                        let chat = workspace.chat(&chat_id).ok().flatten();
                        let device = chat
                            .as_ref()
                            .map(|c| c.device_id.clone())
                            .unwrap_or_default();
                        snapshot.remote = device != catalog.device_id();
                        let cwd = chat.and_then(|c| c.cwd);
                        let cwd = cwd.map(|cwd| {
                            if snapshot.remote {
                                std::path::PathBuf::from(cwd)
                            } else {
                                std::path::PathBuf::from(&cwd)
                                    .canonicalize()
                                    .unwrap_or_else(|_| cwd.into())
                            }
                        });
                        snapshot.project_name = cwd
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|s| s.to_string_lossy().into_owned());
                        snapshot.services.retain(|service| {
                            service.device_id == device
                                && cwd.as_ref().is_some_and(|cwd| {
                                    cwd == std::path::Path::new(&service.project_cwd)
                                })
                        });
                        let value = serde_json::to_value(snapshot).ok()?;
                        Some((value, (changes, chats, false, workspace, catalog, chat_id)))
                    },
                );
                Ok(RpcReply::Stream(Box::pin(stream)))
            }
            methods::WATCH_CHATS => {
                Ok(RpcReply::Stream(watch_stream(self.workspace.watch_chats())))
            }
            methods::WATCH_DEVICES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_devices(),
            ))),
            methods::WATCH_SPACES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_spaces(),
            ))),
            methods::WATCH_SESSIONS => {
                // Local live statuses merged with remote devices' workspace rows.
                let merged = self
                    .workspace
                    .merged_sessions_watch(self.sessions.watch_sessions());
                Ok(RpcReply::Stream(watch_stream(merged)))
            }
            methods::LOCAL_DEVICE => {
                RpcReply::value(&serde_json::json!({ "deviceId": self.doc_host.device_id() }))
            }
            methods::LOCAL_IMPORT_STATUS => {
                let importer = self.local_importer()?.clone();
                let status = tokio::task::spawn_blocking(move || importer.status())
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&status)
            }
            methods::IMPORT_LOCAL_WORKSPACE => {
                let importer = self.local_importer()?.clone();
                // Progress rides an unbounded channel: the importer is
                // blocking (sqlite + fs) and must never wedge on a slow
                // viewer; items are tiny and bounded by the chat count.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
                tokio::task::spawn_blocking(move || {
                    let emit = |event: crate::local_import::ImportEvent| {
                        if let Ok(item) = serde_json::to_value(&event) {
                            let _ = tx.send(item);
                        }
                    };
                    if let Err(err) = importer.run(emit) {
                        tracing::error!(error = %err, "local import failed");
                        let _ = tx.send(serde_json::json!({
                            "kind": "summary",
                            "importedChats": 0, "importedSpaces": 0,
                            "skippedChats": 0, "skippedSpaces": 0,
                            "journalsCopied": 0, "ledgerRowsMerged": 0,
                            "errors": [format!("{err}")],
                        }));
                    }
                    // tx drops here — the stream ends after the summary item.
                });
                Ok(RpcReply::Stream(Box::pin(futures::stream::poll_fn(
                    move |cx| rx.poll_recv(cx),
                ))))
            }
            methods::UPDATE_STATUS => Ok(RpcReply::Stream(watch_stream(self.updater()?.watch()))),
            methods::APPLY_UPDATE => {
                let version = self
                    .updater()?
                    .apply()
                    .await
                    .map_err(|e| RpcError::Failed(format!("{e:#}")))?;
                RpcReply::value(&serde_json::json!({ "ok": true, "version": version }))
            }
            methods::MUTATE => {
                let p: MutateParams = parse_params(params.clone())?;
                match task_intent_from_mutation(p) {
                    Some((task_id, intent)) => {
                        let receipt = self
                            .execute_task_command(legacy_task_command(task_id, intent), true)
                            .await?;
                        RpcReply::value(&serde_json::json!({ "ok": true, "receipt": receipt }))
                    }
                    None => {
                        // Non-task project/device mutations remain on their existing
                        // owner; L07 deliberately does not pretend they are task commands.
                        self.mutate(parse_params(params)?).await?;
                        RpcReply::value(&serde_json::json!({ "ok": true }))
                    }
                }
            }
            methods::WATCH_CHECKOUT_DIFFS => {
                Ok(RpcReply::Stream(watch_stream(self.diff_sync.watch_diffs())))
            }
            methods::GET_GIT_ACTION_PREVIEW => {
                #[derive(Deserialize)]
                struct P {
                    cwd: String,
                }
                let p: P = parse_params(params)?;
                let preview = self
                    .git_actions
                    .preview(&p.cwd)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&preview)
            }
            methods::EXECUTE_GIT_ACTION => {
                let request: zeron_proto::GitActionRequest = parse_params(params)?;
                let receipt = self
                    .git_actions
                    .execute(request)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&receipt)
            }
            methods::GENERATE_GIT_COMMIT_MESSAGE => {
                let request: zeron_proto::GitMessageRequest = parse_params(params)?;
                let preferred_harness = request.harness;
                let diff = self
                    .git_actions
                    .staged_context(request)
                    .await
                    .map_err(RpcError::Failed)?;
                let titles = crate::titles::TitleGenerator::new(
                    self.workspace.clone(),
                    self.registry.clone(),
                    self.repos.clone(),
                );
                let message = titles
                    .generate_commit_message(&diff, preferred_harness)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({ "message": message }))
            }
            methods::GET_GIT_PR_STATUS => {
                Box::pin(async move {
                    let mut request: zeron_proto::GitPrStatusRequest = parse_params(params)?;
                    let root = self.change_request_root(&request.cwd).await?;
                    request.cwd = root.to_string_lossy().into_owned();
                    let status = self
                        .git_actions
                        .pr_status(request)
                        .await
                        .map_err(RpcError::Failed)?;
                    RpcReply::value(&status)
                })
                .await
            }
            methods::WATCH_CHECKOUT_CHANGE_REQUEST => {
                let p: CheckoutChangeRequestParams = parse_params(params)?;
                let cwd = self.change_request_root(&p.cwd).await?;
                let stream = self
                    .change_requests
                    .watch_for_branch(&cwd, p.branch.as_deref())
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .filter_map(|status| async move { serde_json::to_value(status).ok() });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            // One-shot scoped capture for the Changes pane: `branch` diffs the
            // working tree against merge-base(baseRef, HEAD); `turn` diffs the
            // turn-start tree snapshot against the current tree; anything else
            // is the plain working-tree capture.
            methods::GET_CHECKOUT_DIFF => {
                // Keep the scoped-diff future off the dispatcher's stack. The
                // per-commit path adds another nested git-capture future.
                Box::pin(async move {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct P {
                        cwd: String,
                        #[serde(default)]
                        mode: String,
                        base_ref: Option<String>,
                        chat_id: Option<String>,
                        commit_sha: Option<String>,
                    }
                    let p: P = parse_params(params)?;
                    let identity = self
                        .repos
                        .checkout_identity(std::path::Path::new(&p.cwd))
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    let root = identity.root.as_path();
                    let snapshot = match p.mode.as_str() {
                        "branch" => {
                            let base_ref = p
                                .base_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
                            let base = crate::diff_sync::merge_base(root, base_ref)
                                .await
                                .map_err(|e| RpcError::Failed(e.to_string()))?;
                            crate::diff_sync::capture_diff_against(&self.repos, root, Some(&base))
                                .await
                        }
                        // One commit's own changes (History → per-commit tab):
                        // parent (or the empty tree) vs the commit itself.
                        "commit" => {
                            let sha = p
                                .commit_sha
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
                            crate::diff_sync::capture_commit_diff(&self.repos, root, sha).await
                        }
                        "turn" => {
                            let chat_id = p
                                .chat_id
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("chatId required".into()))?;
                            let snapshot = self
                                .diff_sync
                                .turn_snapshot(chat_id)
                                .filter(|s| s.root == identity.root)
                                .ok_or_else(|| RpcError::Failed("no turn recorded".into()))?;
                            crate::diff_sync::capture_turn_diff(&self.repos, root, &snapshot.tree)
                                .await
                        }
                        _ => crate::diff_sync::capture_diff(&self.repos, root).await,
                    }
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                    RpcReply::value(&zeron_proto::CheckoutDiff {
                        checkout_id: identity.id,
                        device_id: self.doc_host.device_id().to_string(),
                        cwd: identity.root.to_string_lossy().to_string(),
                        patch: snapshot.patch,
                        files: snapshot.files,
                        additions: snapshot.additions,
                        deletions: snapshot.deletions,
                        truncated: snapshot.truncated,
                        checksum: snapshot.checksum,
                        updated_at: chrono::Utc::now(),
                    })
                })
                .await
            }
            methods::GET_CHECKOUT_FILE_DIFF_TEXT => {
                // This branch contains several large nested async futures. Keep it
                // behind an allocation so every unrelated RPC does not carry that
                // state in `EngineRpc::handle`'s stack frame.
                Box::pin(async move {
                    let p: zeron_proto::GetCheckoutFileDiffTextRequest = parse_params(params)?;
                    let identity =
                        Box::pin(self.repos.checkout_identity(std::path::Path::new(&p.cwd)))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    if identity.id != p.checkout_id {
                        return Err(RpcError::Failed("checkoutId does not match cwd".into()));
                    }
                    let root = identity.root.as_path();
                    let (snapshot, base, target) = match p.mode.as_str() {
                        "branch" => {
                            let base_ref = p
                                .base_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
                            let base = Box::pin(crate::diff_sync::merge_base(root, base_ref))
                                .await
                                .map_err(|error| RpcError::Failed(error.to_string()))?;
                            let snapshot = Box::pin(crate::diff_sync::capture_diff_against(
                                &self.repos,
                                root,
                                Some(&base),
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, base, None)
                        }
                        "commit" => {
                            let sha = p
                                .commit_sha
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
                            let base =
                                Box::pin(crate::diff_sync::commit_diff_base(root, sha)).await;
                            let snapshot = Box::pin(crate::diff_sync::capture_commit_diff(
                                &self.repos,
                                root,
                                sha,
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, base, Some(sha.to_string()))
                        }
                        "turn" => {
                            let chat_id = p
                                .chat_id
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("chatId required".into()))?;
                            let turn = self
                                .diff_sync
                                .turn_snapshot(chat_id)
                                .filter(|snapshot| snapshot.root == identity.root)
                                .ok_or_else(|| RpcError::Failed("no turn recorded".into()))?;
                            let snapshot = Box::pin(crate::diff_sync::capture_turn_diff(
                                &self.repos,
                                root,
                                &turn.tree,
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, turn.tree, None)
                        }
                        _ => {
                            let base = Box::pin(crate::diff_sync::working_diff_base(root))
                                .await
                                .map_err(|error| RpcError::Failed(error.to_string()))?;
                            let snapshot =
                                Box::pin(crate::diff_sync::capture_diff(&self.repos, root))
                                    .await
                                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, base, None)
                        }
                    };
                    let stale = || zeron_proto::CheckoutFileDiffText {
                        diff_checksum: p.diff_checksum.clone(),
                        old_text: None,
                        new_text: None,
                        old_content_hash: None,
                        new_content_hash: None,
                        binary: false,
                        truncated: false,
                        stale: true,
                    };
                    if snapshot.checksum != p.diff_checksum {
                        return RpcReply::value(&stale());
                    }
                    let file = snapshot
                        .files
                        .iter()
                        .find(|file| file.path == p.path)
                        .ok_or_else(|| {
                            RpcError::Failed("path is not part of diff snapshot".into())
                        })?;
                    let pair = Box::pin(crate::diff_sync::read_diff_file_text_at(
                        root,
                        &base,
                        target.as_deref(),
                        file,
                    ))
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                    let current = match p.mode.as_str() {
                        "branch" => {
                            Box::pin(crate::diff_sync::capture_diff_against(
                                &self.repos,
                                root,
                                Some(&base),
                            ))
                            .await
                        }
                        "turn" => {
                            Box::pin(crate::diff_sync::capture_turn_diff(
                                &self.repos,
                                root,
                                &base,
                            ))
                            .await
                        }
                        "commit" => {
                            let sha = p
                                .commit_sha
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
                            Box::pin(crate::diff_sync::capture_commit_diff(
                                &self.repos,
                                root,
                                sha,
                            ))
                            .await
                        }
                        _ => Box::pin(crate::diff_sync::capture_diff(&self.repos, root)).await,
                    }
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                    if current.checksum != p.diff_checksum {
                        return RpcReply::value(&stale());
                    }
                    RpcReply::value(&zeron_proto::CheckoutFileDiffText {
                        diff_checksum: p.diff_checksum,
                        old_text: pair.old_text,
                        new_text: pair.new_text,
                        old_content_hash: pair.old_content_hash,
                        new_content_hash: pair.new_content_hash,
                        binary: pair.binary,
                        truncated: pair.truncated,
                        stale: false,
                    })
                })
                .await
            }
            methods::LIST_REPOS => RpcReply::value(&self.repos.list().await),
            methods::ADD_REPO => {
                #[derive(Deserialize)]
                struct P {
                    path: String,
                }
                let p: P = parse_params(params)?;
                let repo = self
                    .repos
                    .add(&p.path)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&repo)
            }
            methods::CLONE_REPO => {
                #[derive(Deserialize)]
                struct P {
                    url: String,
                }
                let p: P = parse_params(params)?;
                let repo = self
                    .repos
                    .clone_repo(&p.url)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&repo)
            }
            methods::CREATE_REPO => {
                #[derive(Deserialize)]
                struct P {
                    name: String,
                }
                let p: P = parse_params(params)?;
                let repo = self
                    .repos
                    .create(&p.name)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&repo)
            }
            methods::LIST_BRANCHES => {
                let p: RepoPathParams = parse_params(params)?;
                let branches = self
                    .repos
                    .branches(std::path::Path::new(&p.repo_path))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&branches)
            }
            methods::LIST_REFS => {
                let p: RepoPathParams = parse_params(params)?;
                let refs = self
                    .repos
                    .refs(std::path::Path::new(&p.repo_path))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&refs)
            }
            methods::LIST_GIT_HISTORY => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    #[serde(default)]
                    cursor: usize,
                    #[serde(default = "default_git_history_limit")]
                    limit: usize,
                }
                fn default_git_history_limit() -> usize {
                    crate::repos::GIT_HISTORY_DEFAULT_LIMIT
                }
                let p: P = parse_params(params)?;
                let history = self
                    .repos
                    .history(std::path::Path::new(&p.cwd), p.cursor, p.limit)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&history)
            }
            methods::SEARCH_GIT_HISTORY => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    query: String,
                    #[serde(default)]
                    cursor: usize,
                    #[serde(default = "default_git_history_search_limit")]
                    limit: usize,
                }
                fn default_git_history_search_limit() -> usize {
                    crate::repos::GIT_HISTORY_DEFAULT_LIMIT
                }
                let p: P = parse_params(params)?;
                let history = self
                    .repos
                    .search_history(std::path::Path::new(&p.cwd), &p.query, p.cursor, p.limit)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&history)
            }
            methods::RESOLVE_GIT_AVATARS => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    authors: Vec<GitAvatarAuthor>,
                    #[serde(default)]
                    cursor: usize,
                    #[serde(default = "default_git_avatar_limit")]
                    limit: usize,
                }
                #[derive(Deserialize)]
                struct GitAvatarAuthor {
                    sha: String,
                    email: String,
                }
                fn default_git_avatar_limit() -> usize {
                    crate::repos::GIT_HISTORY_DEFAULT_LIMIT
                }
                let p: P = parse_params(params)?;
                let authors: Vec<_> = p
                    .authors
                    .into_iter()
                    .take(crate::repos::GIT_HISTORY_MAX_LIMIT)
                    .filter(|author| author.sha.len() <= 64 && author.email.len() <= 512)
                    .map(|author| (author.sha, author.email))
                    .collect();
                let avatar_paths = self
                    .repos
                    .history_avatar_urls(std::path::Path::new(&p.cwd), &authors, p.cursor, p.limit)
                    .await;
                let mut avatars = std::collections::HashMap::new();
                for (email, path) in avatar_paths {
                    if let Ok(bytes) = tokio::fs::read(path).await {
                        avatars.insert(
                            email,
                            base64::engine::general_purpose::STANDARD.encode(bytes),
                        );
                    }
                }
                RpcReply::value(&avatars)
            }
            methods::FETCH_ALL => {
                let p: RepoPathParams = parse_params(params)?;
                self.repos
                    .fetch_all(std::path::Path::new(&p.repo_path))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Remote refs are repository state too. Force the checkout
                // watchers to publish a fresh snapshot instead of waiting for
                // the repair tick (some platforms do not report packed-refs).
                self.diff_sync.sync_all();
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SWITCH_REF => {
                let p: SwitchRefParams = parse_params(params)?;
                let branch = self
                    .repos
                    .switch_ref(std::path::Path::new(&p.repo_path), &p.ref_name)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "branch": branch }))
            }
            methods::LIST_FOLDERS => {
                let p: ListFoldersParams = parse_params(params)?;
                let listing = self
                    .repos
                    .list_folders(p.path)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&listing)
            }
            methods::LIST_DRIVES => {
                let drives = self
                    .repos
                    .list_drives()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&zeron_proto::DriveListing { drives })
            }
            methods::SEARCH_FILES => {
                let p: FileSearchParams = parse_params(params)?;
                if p.query.chars().count() > 256 {
                    return Err(RpcError::BadParams(
                        "SearchFiles query must not exceed 256 characters".into(),
                    ));
                }
                let matches = tokio::time::timeout(FILE_SEARCH_RPC_TIMEOUT, async {
                    let root = self.file_search_root(&p).await?;
                    let featured_paths = p
                        .chat_id
                        .as_deref()
                        .filter(|_| p.query.is_empty())
                        .map(|chat_id| self.featured_file_paths(chat_id))
                        .unwrap_or_default();
                    self.repos
                        .search_files(root, p.query, featured_paths)
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))
                })
                .await
                .map_err(|_| RpcError::Failed("file search timed out".into()))??;
                RpcReply::value(&matches)
            }
            methods::LIST_WORKSPACE_DIRECTORY => {
                let request: zeron_proto::ListWorkspaceDirectoryRequest = parse_params(params)?;
                let page = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.list_directory(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace directory listing timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&page)
            }
            methods::SEARCH_WORKSPACE_FILES => {
                let request: zeron_proto::SearchWorkspaceFilesRequest = parse_params(params)?;
                let matches = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.search(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace file search timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&matches)
            }
            methods::READ_WORKSPACE_IMAGE => {
                let request: zeron_proto::ReadWorkspaceImageRequest = parse_params(params)?;
                let chunk = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.read_image(request),
                )
                .await
                .map_err(|_| RpcError::Failed("Workspace image read timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&chunk)
            }
            methods::READ_WORKSPACE_FILE => {
                let request: zeron_proto::ReadWorkspaceFileRequest = parse_params(params)?;
                let file = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.read_file(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace file read timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&file)
            }
            methods::WRITE_WORKSPACE_FILE => {
                let request: zeron_proto::WriteWorkspaceFileRequest = parse_params(params)?;
                let outcome = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.write_file(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace file write timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&outcome)
            }
            methods::WATCH_WORKSPACE_FILES => {
                let request: zeron_proto::WatchWorkspaceFilesRequest = parse_params(params)?;
                let subscription = self
                    .workspace_files
                    .watch_files(request)
                    .await
                    .map_err(RpcError::from)?;
                let stream = futures::stream::unfold(subscription, |mut subscription| async move {
                    let changes = subscription.recv().await?;
                    let value = serde_json::to_value(changes).ok()?;
                    Some((value, subscription))
                });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::CREATE_WORKTREE => {
                let p: CreateWorktreeParams = parse_params(params)?;
                let worktree = self
                    .repos
                    .create_worktree(std::path::Path::new(&p.repo_path), &p.branch)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&worktree)
            }
            methods::DELETE_WORKTREE => {
                let p: DeleteWorktreeParams = parse_params(params)?;
                self.repos
                    .delete_worktree(
                        std::path::Path::new(&p.repo_path),
                        std::path::Path::new(&p.worktree_path),
                    )
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::OPEN_TERMINAL => {
                let p: OpenTerminalParams = parse_params(params)?;
                // The terminal runs in the chat's checkout; a chat with no cwd (or
                // no row yet) gets the home directory.
                let cwd = self
                    .workspace
                    .chat(&p.chat_id)
                    .ok()
                    .flatten()
                    .and_then(|chat| chat.cwd)
                    .unwrap_or_else(|| home_dir().to_string_lossy().to_string());
                let session = self
                    .terminals
                    .open_for_task(&p.chat_id, &cwd, p.cols, p.rows)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&session)
            }
            methods::SUBSCRIBE_TERMINAL => {
                let p: SubscribeTerminalParams = parse_params(params)?;
                let rx = self
                    .terminals
                    .subscribe(&p.terminal_id, p.after_seq)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let stream = futures::stream::unfold(rx, |mut rx| async move {
                    let event = rx.recv().await?;
                    let value = serde_json::to_value(&event).ok()?;
                    Some((value, rx))
                });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::WRITE_TERMINAL => {
                let p: WriteTerminalParams = parse_params(params)?;
                self.terminals
                    .write(&p.terminal_id, &p.data)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::RESIZE_TERMINAL => {
                let p: ResizeTerminalParams = parse_params(params)?;
                self.terminals
                    .resize(&p.terminal_id, p.cols, p.rows)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::CLOSE_TERMINAL => {
                let p: TerminalIdParams = parse_params(params)?;
                self.terminals
                    .close(&p.terminal_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LIST_TERMINALS => {
                let p: ListTerminalsParams = parse_params(params)?;
                let entries = self
                    .terminals
                    .list(p.chat_id.as_deref())
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&entries)
            }
            methods::LIST_AGENT_ACCOUNTS => {
                let p: ListAgentAccountsParams = parse_params(params)?;
                let snapshot = match p.harness {
                    Some(harness) => {
                        self.agent_accounts
                            .list_harness(harness, p.force_usage.unwrap_or(false))
                            .await
                    }
                    None => {
                        self.agent_accounts
                            .list(p.force_usage.unwrap_or(false))
                            .await
                    }
                }
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::ACTIVATE_AGENT_ACCOUNT => {
                let p: AgentAccountParams = parse_params(params)?;
                let _transition = self
                    .sessions
                    .prepare_provider_account_change(p.harness)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let snapshot = self
                    .agent_accounts
                    .activate(p.harness, &p.account_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::RENAME_AGENT_ACCOUNT => {
                let p: RenameAgentAccountParams = parse_params(params)?;
                let snapshot = self
                    .agent_accounts
                    .rename(p.harness, &p.account_id, &p.label)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::FORGET_AGENT_ACCOUNT => {
                let p: AgentAccountParams = parse_params(params)?;
                let _transition = self
                    .sessions
                    .prepare_provider_account_change(p.harness)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let snapshot = self
                    .agent_accounts
                    .forget(p.harness, &p.account_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::START_AGENT_LOGIN => {
                let p: StartAgentLoginParams = parse_params(params)?;
                let start = self
                    .agent_accounts
                    .start_login(p.harness)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&start)
            }
            methods::COMPLETE_AGENT_LOGIN => {
                let p: CompleteAgentLoginParams = parse_params(params)?;
                let harness = self
                    .agent_accounts
                    .login_harness(&p.login_id)
                    .ok_or_else(|| {
                        RpcError::Failed("This sign-in attempt expired — start again.".into())
                    })?;
                let _transition = self
                    .sessions
                    .prepare_provider_account_change(harness)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let snapshot = self
                    .agent_accounts
                    .complete_login(&p.login_id, &p.code)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::POLL_AGENT_LOGIN => {
                let p: LoginIdParams = parse_params(params)?;
                let poll = self
                    .agent_accounts
                    .poll_login(&p.login_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&poll)
            }
            methods::CANCEL_AGENT_LOGIN => {
                let p: LoginIdParams = parse_params(params)?;
                self.agent_accounts.cancel_login(&p.login_id);
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::UPLOAD_CHUNK => {
                let p: UploadChunkParams = parse_params(params)?;
                self.uploads
                    .append(&p.upload_id, &p.data, p.seq)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::UPLOAD_COMMIT => {
                let p: UploadCommitParams = parse_params(params)?;
                let path = self
                    .uploads
                    .commit(&p.upload_id, &p.file_name)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Bytes just landed on this device: any command deferred on
                // them (queued-attachment refs) is executable NOW.
                self.doc_host.kick_drains();
                RpcReply::value(&serde_json::json!({ "path": path }))
            }
            methods::READ_ATTACHMENT_CHUNK => {
                let p: ReadAttachmentChunkParams = parse_params(params)?;
                // Path jail: the uploads dir plus every workspace-known chat cwd.
                let roots: Vec<std::path::PathBuf> = self
                    .workspace
                    .read_chats()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|chat| chat.cwd)
                    .map(std::path::PathBuf::from)
                    .collect();
                let chunk = self
                    .uploads
                    .read_chunk(&p.path, p.offset, &roots)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&chunk)
            }
            methods::FETCH_TOOL_BLOB => {
                let p: FetchToolBlobParams = parse_params(params)?;
                let text = self
                    .doc_host
                    .fetch_tool_blob(&p.blob_ref)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "text": text }))
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_task_queue_uses_one_stable_id_and_waits_for_run_end() {
        let authority = CrossTaskAuthority {
            caller_task_id: "caller-task".into(),
            local_environment_id: "synthetic-environment".into(),
            visible_project_ids: HashSet::from(["synthetic-project".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.send".into()]),
            explicit_user_request: true,
            delivery: zeron_proto::RuntimeDelivery::CodexDynamicTools,
        };
        let request = zeron_proto::CrossTaskSendRequest {
            task_id: "target-task".into(),
            message: "synthetic follow-up".into(),
            mode: zeron_proto::CrossTaskSendMode::Queue,
            idempotency_key: "command-a".into(),
        };
        let command = cross_task_queue_command(&request, &authority);
        assert_eq!(command.command_id, "command-a");
        assert_eq!(command.task_id, "target-task");
        assert_eq!(command.actor_id, "luna-tool:caller-task");
        assert!(matches!(command.intent, TaskCommandIntent::QueueMessage {
            message_id, hold_for_turn_end: true,
            follow_up_behavior: FollowUpBehavior::Queue, attachments,
            ..
        } if message_id == "command-a" && attachments.is_empty()));
    }

    #[test]
    fn integration_reads_do_not_silently_use_the_wrong_device() {
        assert!(require_local_integration_target(None, "local").is_ok());
        assert!(require_local_integration_target(Some("local"), "local").is_ok());
        assert!(require_local_integration_target(Some("remote"), "local").is_err());
    }

    fn move_test_space(path: &str) -> zeron_proto::Space {
        zeron_proto::Space {
            id: "destination".into(),
            device_id: "device".into(),
            path: path.into(),
            name: Some("Destination".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
        }
    }

    fn move_test_chat() -> Chat {
        Chat {
            id: "task".into(),
            device_id: "device".into(),
            title: Some("Task".into()),
            archived: false,
            pinned_order: None,
            cwd: Some("/workspace".into()),
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            harness_session_id: Some("provider-session".into()),
            harness_session_cwd: Some("/workspace".into()),
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: Some(2),
        }
    }

    #[test]
    fn validated_move_rejects_changed_or_missing_topology_without_relabeling() {
        let mut changed = RegistryDoc::new("device");
        let destination = move_test_space("/workspace");
        changed.upsert_space(&destination).unwrap();
        changed.upsert_chat(&move_test_chat()).unwrap();
        let validated = ValidatedProjectMove {
            destination: destination.clone(),
        };
        changed
            .upsert_space(&move_test_space("/workspace/replaced"))
            .unwrap();
        assert!(commit_validated_project_move(&mut changed, "task", &validated).is_err());
        assert_eq!(changed.chat("task").unwrap().unwrap().space_id, None);

        let mut missing = RegistryDoc::new("device");
        missing.upsert_space(&destination).unwrap();
        missing.upsert_chat(&move_test_chat()).unwrap();
        missing.delete_space("destination").unwrap();
        assert!(commit_validated_project_move(&mut missing, "task", &validated).is_err());
        assert_eq!(missing.chat("task").unwrap().unwrap().space_id, None);
    }

    #[tokio::test]
    async fn antigravity_sign_out_failure_stays_enabled_and_can_be_retried() {
        let registry = HarnessRegistry::new();
        let executable = std::env::current_exe().unwrap();
        registry.register(std::sync::Arc::new(
            zeron_harness::AcpHarness::grok().with_executable(executable.clone()),
        ));
        registry.register(std::sync::Arc::new(
            zeron_harness::AcpHarness::antigravity().with_executable(executable),
        ));
        registry.set_enabled(HarnessId::Antigravity, true).unwrap();

        let error = update_harness_enabled(&registry, HarnessId::Antigravity, false, async {
            Err(zeron_harness::HarnessError::Protocol(
                "logout rejected".into(),
            ))
        })
        .await
        .unwrap_err();
        assert!(
            matches!(error, RpcError::Failed(ref message) if message.contains("remains enabled")),
            "{error}"
        );
        assert!(registry.enabled_set().contains(&HarnessId::Antigravity));

        update_harness_enabled(&registry, HarnessId::Antigravity, false, async {
            Ok::<(), zeron_harness::HarnessError>(())
        })
        .await
        .unwrap();
        assert!(!registry.enabled_set().contains(&HarnessId::Antigravity));
    }

    /// The UI's Switch/Forget calls send `{id, accountId, harness}` (+ optional
    /// `targetDeviceId`); the extra fields must be tolerated, `accountId` wins.
    #[test]
    fn agent_account_params_accept_ui_shape() {
        let p: AgentAccountParams = parse_params(serde_json::json!({
            "id": "acct-1",
            "accountId": "acct-1",
            "harness": "claude-code",
            "targetDeviceId": "dev-2",
        }))
        .expect("ui param shape");
        assert_eq!(p.account_id, "acct-1");
        assert_eq!(p.harness, HarnessId::ClaudeCode);
    }

    #[test]
    fn local_device_is_not_forwardable() {
        assert!(!forwardable(methods::LOCAL_DEVICE));
        assert!(!forwardable(methods::ENGINE_INFO));
        assert!(!forwardable(methods::ENGINE_READY));
        assert!(!forwardable(methods::START_AGENT_LOGIN));
        assert!(!forwardable(methods::COMPLETE_AGENT_LOGIN));
        assert!(local_only_credential_method(methods::START_AGENT_LOGIN));
        assert!(local_only_credential_method(methods::SIGN_IN));
        assert!(local_only_credential_method(methods::SIGN_OUT));
        assert!(local_only_credential_method(
            methods::LIST_CREDENTIAL_METADATA
        ));
        assert!(forwardable(methods::QUEUE_COMMAND));
        assert!(forwardable(methods::EXECUTE_TASK_COMMAND));
        assert!(forwardable(methods::GET_TASK_COMMAND_RECEIPT));
        assert!(forwardable(methods::GET_TASK_COMMAND_SNAPSHOT));
        assert!(forwardable(methods::GET_RUNTIME_CONTRACT));
        assert!(forwardable(methods::WATCH_TASK_COMMANDS));
        assert!(is_stream_method(methods::WATCH_TASK_COMMANDS));
        assert!(forwardable(methods::SEARCH_FILES));
        assert!(forwardable(methods::SEARCH_GIT_HISTORY));
        assert!(forwardable(methods::FETCH_ALL));
        assert!(forwardable(methods::RESOLVE_GIT_AVATARS));
        assert!(forwardable(methods::WATCH_CHECKOUT_CHANGE_REQUEST));
        assert!(is_stream_method(methods::WATCH_CHECKOUT_CHANGE_REQUEST));
        assert!(forwardable(methods::LIST_WORKSPACE_DIRECTORY));
        assert!(forwardable(methods::SEARCH_WORKSPACE_FILES));
        assert!(forwardable(methods::READ_WORKSPACE_FILE));
        assert!(forwardable(methods::READ_WORKSPACE_IMAGE));
        assert!(forwardable(methods::WRITE_WORKSPACE_FILE));
        assert!(forwardable(methods::WATCH_WORKSPACE_FILES));
        assert!(!is_stream_method(methods::LIST_WORKSPACE_DIRECTORY));
        assert!(!is_stream_method(methods::SEARCH_WORKSPACE_FILES));
        assert!(!is_stream_method(methods::READ_WORKSPACE_FILE));
        assert!(!is_stream_method(methods::WRITE_WORKSPACE_FILE));
        assert!(is_stream_method(methods::WATCH_WORKSPACE_FILES));
    }

    /// Every forwardable unary method gets a bounded reply deadline —
    /// interactive calls fail fast, network-bound git/update calls get the
    /// long leash, and nothing awaits forever (the "Sending…" wedge).
    #[test]
    fn forward_deadlines_are_tiered_and_bounded() {
        use std::time::Duration;
        assert_eq!(
            forward_deadline(methods::CREATE_WORKTREE),
            Duration::from_secs(120)
        );
        assert_eq!(
            forward_deadline(methods::CLONE_REPO),
            Duration::from_secs(15 * 60)
        );
        assert_eq!(
            forward_deadline(methods::LIST_BRANCHES),
            Duration::from_secs(30)
        );
        assert_eq!(
            forward_deadline(methods::QUEUE_COMMAND),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn tool_file_paths_keep_workspace_activity_only() {
        assert_eq!(
            tool_file_path(&ToolCall::EditFile {
                path: "src/main.rs".into(),
                old_string: None,
                new_string: None,
            }),
            Some("src/main.rs")
        );
        assert_eq!(
            tool_file_path(&ToolCall::Exec {
                command: "cargo test".into(),
            }),
            None
        );
    }
}

#[cfg(test)]
mod context_usage_tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Arc;

    #[tokio::test]
    async fn replay_cutoff_travels_with_coalesced_backfill_and_live_content() {
        use crate::doc_host::{DocHost, DocHostConfig};
        use zeron_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(zeron_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: zeron_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("replay-chat").unwrap();
        let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "replay-chat")
            .with_handle(Arc::downgrade(&handle));
        let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        let first: zeron_doc::TranscriptUpdate =
            serde_json::from_value(stream.next().await.unwrap()).unwrap();
        assert!(first.replay_baseline.unwrap().entries.is_empty());

        let source = zeron_doc::SessionDoc::init("replay-chat").unwrap();
        let append = |id: &str| {
            source
                .push_message(&zeron_doc::SessionMessageEntry {
                    id: id.into(),
                    role: zeron_doc::MessageRole::Assistant,
                    parts: vec![zeron_doc::MessagePart::Text {
                        id: "text".into(),
                        text: id.into(),
                    }],
                    created_at: 0,
                    device_id: "writer".into(),
                    status: Some(zeron_doc::MessageStatus::Streaming),
                    continuation_of: None,
                })
                .unwrap()
        };
        append("cached");
        sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
            .unwrap();
        let checkpoint: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            checkpoint
                .replay_baseline
                .unwrap()
                .entries
                .contains_key("cached")
        );

        let version = source.doc().oplog_vv();
        append("away");
        sink.apply_replay_row(
            &source
                .doc()
                .export(loro::ExportMode::updates(&version))
                .unwrap(),
            1,
        );
        let version = source.doc().oplog_vv();
        append("live");
        sink.apply_row(
            &source
                .doc()
                .export(loro::ExportMode::updates(&version))
                .unwrap(),
            2,
        );
        // Neither the doc worker nor the RPC consumer ran between these
        // imports. They must not flatten their different presentation origins.
        let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let cutoff = update.replay_baseline.unwrap();
        assert!(cutoff.entries.contains_key("away"));
        assert!(!cutoff.entries.contains_key("live"));
        let mut entries = vec![];
        zeron_doc::apply_transcript_frame(&mut entries, checkpoint.frame).unwrap();
        zeron_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
        assert_eq!(entries.len(), 3);

        let version = source.doc().oplog_vv();
        append("next-live");
        sink.apply_row(
            &source
                .doc()
                .export(loro::ExportMode::updates(&version))
                .unwrap(),
            3,
        );
        let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            update.replay_baseline.is_none(),
            "live updates must not resend the history watermark"
        );
        let mut reopened = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        let opening: zeron_doc::TranscriptUpdate =
            serde_json::from_value(reopened.next().await.unwrap()).unwrap();
        assert_eq!(
            opening.replay_baseline.unwrap().entries.len(),
            4,
            "reopening includes all existing content as history"
        );
        host.shutdown_workers().await;
    }

    #[tokio::test]
    async fn replay_metadata_and_backfill_leave_interleaved_local_content_live() {
        use crate::doc_host::{DocHost, DocHostConfig};
        use zeron_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(zeron_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "host".into(),
                default_harness: zeron_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("interleaved").unwrap();
        let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "interleaved")
            .with_handle(Arc::downgrade(&handle));
        let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        stream.next().await.unwrap();
        let source = zeron_doc::SessionDoc::init("interleaved").unwrap();
        sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
            .unwrap();
        let entry = |id: &str| zeron_doc::SessionMessageEntry {
            id: id.into(),
            role: zeron_doc::MessageRole::Assistant,
            parts: vec![zeron_doc::MessagePart::Text {
                id: "text".into(),
                text: id.into(),
            }],
            created_at: 0,
            device_id: "host".into(),
            status: Some(zeron_doc::MessageStatus::Streaming),
            continuation_of: None,
        };
        handle.doc().push_message(&entry("local-before")).unwrap();
        source.update_context_usage(Some(10), Some(100)).unwrap();
        sink.apply_replay_row(&source.export_snapshot().unwrap(), 1);
        let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            update.replay_baseline.is_none(),
            "metadata must not reset ongoing live animations"
        );
        let mut entries = vec![];
        zeron_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
        assert_eq!(entries[0].id, "local-before");
        let version = source.doc().oplog_vv();
        source.push_message(&entry("historical")).unwrap();
        handle.doc().push_message(&entry("local-between")).unwrap();
        sink.apply_replay_row(
            &source
                .doc()
                .export(loro::ExportMode::updates(&version))
                .unwrap(),
            2,
        );
        handle.doc().push_message(&entry("local-after")).unwrap();
        let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let baseline = update.replay_baseline.unwrap();
        assert_eq!(baseline.entries.len(), 1);
        assert!(baseline.entries.contains_key("historical"));
        zeron_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
        assert_eq!(entries.len(), 4);
        host.shutdown_workers().await;
    }

    #[tokio::test]
    async fn replay_preserves_each_watchers_opening_cutoff_without_consuming_live_text() {
        use crate::doc_host::{DocHost, DocHostConfig};
        use zeron_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(zeron_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: zeron_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("cached-replay").unwrap();
        let sink =
            crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "cached-replay")
                .with_handle(Arc::downgrade(&handle));
        let source = zeron_doc::SessionDoc::init("cached-replay").unwrap();
        let mut writer = zeron_doc::SegmentWriter::begin(&source, "reply", "host", 0).unwrap();
        let text = |id: &str, value: &str| zeron_doc::MessagePart::Text {
            id: id.into(),
            text: value.into(),
        };
        let cached = text("body", "café histórico");
        writer.sync(&[cached.clone()]).unwrap();
        sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
            .unwrap();
        // Cached content exists before the first watcher and never enters
        // the changed-parts tracker. It may not have been painted yet.
        let mut first = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        let opening: zeron_doc::TranscriptUpdate =
            serde_json::from_value(first.next().await.unwrap()).unwrap();
        assert_eq!(
            opening.replay_baseline.unwrap().entries["reply"]["body"],
            "café histórico".len()
        );

        let live = text("body", "café histórico y nuevo");
        writer.sync(&[live.clone()]).unwrap();
        sink.apply_row(&source.export_snapshot().unwrap(), 1);
        let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), first.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(update.replay_baseline.is_none());
        // A later subscriber sees a longer historical prefix, but must not
        // change the first subscriber's ongoing live animation.
        let mut second = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        let opening: zeron_doc::TranscriptUpdate =
            serde_json::from_value(second.next().await.unwrap()).unwrap();
        assert_eq!(
            opening.replay_baseline.unwrap().entries["reply"]["body"],
            "café histórico y nuevo".len()
        );

        let mut parts = vec![live];
        for ix in 0..2 {
            let id = format!("recovered-{ix}");
            parts.push(text(&id, "otro bloque histórico"));
            writer.sync(&parts).unwrap();
            sink.apply_replay_row(&source.export_snapshot().unwrap(), 2 + ix);
            for (stream, expected) in [
                (&mut first, "café histórico".len()),
                (&mut second, "café histórico y nuevo".len()),
            ] {
                let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
                    tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                        .await
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                let baseline = update.replay_baseline.unwrap();
                assert_eq!(
                    baseline.entries["reply"].get("body"),
                    Some(&expected),
                    "replay must retain this watcher's opening cutoff, excluding later live bytes"
                );
                assert_eq!(
                    baseline.entries["reply"][&id],
                    "otro bloque histórico".len()
                );
                assert_eq!(baseline.entries["reply"].len(), 2 + ix as usize);
            }
        }
        host.shutdown_workers().await;
    }

    #[tokio::test]
    async fn reopening_rearms_history_for_previously_live_text() {
        use crate::doc_host::{DocHost, DocHostConfig};
        use zeron_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(zeron_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: zeron_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("reopen").unwrap();
        let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "reopen")
            .with_handle(Arc::downgrade(&handle));
        let source = zeron_doc::SessionDoc::init("reopen").unwrap();
        let mut writer = zeron_doc::SegmentWriter::begin(&source, "reply", "host", 0).unwrap();
        let part = |text: &str| zeron_doc::MessagePart::Text {
            id: "body".into(),
            text: text.into(),
        };
        let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        stream.next().await.unwrap();
        writer.sync(&[part("live")]).unwrap();
        sink.apply_row(&source.export_snapshot().unwrap(), 1);
        let _: serde_json::Value =
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap();
        drop(stream);
        // No unwatched commit clears provenance before the new attach.
        let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        stream.next().await.unwrap();
        writer.sync(&[part("live plus recovered")]).unwrap();
        sink.apply_replay_row(&source.export_snapshot().unwrap(), 2);
        let update: zeron_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            update.replay_baseline.unwrap().entries["reply"]["body"],
            "live plus recovered".len()
        );
        host.shutdown_workers().await;
    }

    #[tokio::test]
    async fn context_only_commits_reach_remote_watch_and_reconnect() {
        let host = zeron_doc::SessionDoc::init("context-chat").unwrap();
        host.update_context_usage(Some(42000), Some(200000))
            .unwrap();
        // The viewing engine reads a replicated document, with no harness process.
        let remote = Arc::new(zeron_doc::SessionDoc::from_doc(loro::LoroDoc::new()));
        remote
            .doc()
            .import(&host.export_snapshot().unwrap())
            .unwrap();
        let (tx, rx) = watch::channel(crate::doc_host::TranscriptSnapshot::default());
        let mut stream = doc_messages_stream(rx, remote.clone());
        let first = stream.next().await.unwrap();
        assert_eq!(first["contextUsage"]["tokens"], 42000);
        assert!(first.get("reset").is_some());
        let version = host.doc().oplog_vv();
        host.update_context_usage(Some(0), None).unwrap();
        remote
            .doc()
            .import(
                &host
                    .doc()
                    .export(loro::ExportMode::updates(&version))
                    .unwrap(),
            )
            .unwrap();
        tx.send_replace(crate::doc_host::TranscriptSnapshot::default());
        let update = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(update["contextUsage"]["tokens"], 0);
        assert_eq!(update["contextUsage"]["window"], 200000);
        let mut reconnect = doc_messages_stream(tx.subscribe(), remote.clone());
        assert_eq!(
            reconnect.next().await.unwrap()["contextUsage"],
            update["contextUsage"]
        );
        remote.clear_context_usage().unwrap();
        tx.send_replace(crate::doc_host::TranscriptSnapshot::default());
        assert!(stream.next().await.unwrap()["contextUsage"].is_null());
    }
}
