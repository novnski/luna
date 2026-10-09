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
//! Selected ControlRpc methods are device-addressable: params may carry `targetDeviceId`. When it
//! names another device, the call is forwarded verbatim over that device's relay DO via
//! the [`LinkCache`] — the remote engine sees its own id and handles locally, so the
//! forward can never loop. Streaming methods are proxied by re-subscribing remotely and
//! piping items. To make another method device-addressable, nothing per-method is needed
//! beyond listing it in [`forwardable`] (and [`is_stream_method`] if it streams);
//! handlers stay transport-agnostic, while L29's remote dispatch still requires
//! a paired session and an explicit method scope. This includes the workspace file surface,
//! whose checkout always lives on the routed target device.

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, RegistryDoc, SessionCommandEntry,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry,
};
use luna_proto::{
    AttentionMutationRequest, AttentionMutationResult, AttentionProducerAvailability,
    AttentionReadRequest, AttentionSnapshot, AttentionTransitionBatch, AttentionTransitionClaim,
    BackupArchiveRequest, BackupAvailability, BackupConfigureRequest, BackupStatus, BackupSummary,
    BindingOrigin, Chat, ChatConfig, CommandReasonCode, CommandReceipt, CommandReceiptReason,
    CommandReceiptStatus, CompactOperation, ComposerDraft, ComposerDraftRead, ContextBoundary,
    ContextBoundaryKind, ContextControlState, ContextHandoff, ContextOperationStatus,
    ConversationForkBoundaryKind, DraftBlobState, EngineInfo, FollowUpBehavior, HarnessId,
    HistoryCapabilityView, HistoryCursor, HistoryProjectPolicy, HistoryRebuildRequest,
    HistoryRebuildResult, HistoryRecoveryView, HistorySearchRequest, ImportOwnership,
    PermissionMode, ProviderForkCapability, ProviderForkRequest, TaskCommandEvent,
    TaskCommandSnapshot, TaskLineage, ToolCall, UsageRequest, WorkspaceScope,
};
use luna_rpc::{LinkCache, RpcError, RpcReply, RpcService, methods, parse_params};
use luna_sync::{DocsStore, LocalExecutableChange, LocalExecutableKind};

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
struct TranscriptWatchParams {
    chat_id: String,
    #[serde(default)]
    transcript_resume: bool,
    #[serde(default)]
    known_transcript_token: Option<String>,
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
    /// Selected chat whose own provider session report may mark attachment.
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    target_device_id: Option<String>,
    #[serde(default)]
    probe_provider: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDocumentParams {
    provider: HarnessId,
    #[serde(default)]
    project_id: Option<String>,
    identity: String,
    #[serde(default)]
    target_device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSkillDocumentParams {
    provider: HarnessId,
    #[serde(default)]
    project_id: Option<String>,
    scope: luna_proto::IntegrationSkillScope,
    name: String,
    content: String,
    #[serde(default)]
    expected_revision: Option<String>,
    #[serde(default)]
    target_device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallSkillParams {
    source_provider: HarnessId,
    #[serde(default)]
    project_id: Option<String>,
    source_identity: String,
    expected_source_revision: String,
    destination_provider: HarnessId,
    destination_scope: luna_proto::IntegrationSkillScope,
    #[serde(default)]
    target_device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveSkillParams {
    provider: HarnessId,
    #[serde(default)]
    project_id: Option<String>,
    identity: String,
    expected_revision: String,
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
    #[serde(default)]
    refresh: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetHarnessEnabledParams {
    harness: HarnessId,
    enabled: bool,
}

fn update_harness_enabled(
    registry: &HarnessRegistry,
    harness: HarnessId,
    enabled: bool,
) -> Result<(), RpcError> {
    let permit = registry
        .admit_prefs_write()
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    registry
        .set_enabled_with_permit(harness, enabled, permit.as_ref())
        .map_err(|error| RpcError::Failed(error.to_string()))
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

/// Engine-owned proof of provider turns inherited by a fork but intentionally
/// absent from its visible conversation. Scoped to one provider session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct HiddenTurnPrefix {
    provider: HarnessId,
    provider_account_id: Option<String>,
    cwd: String,
    turns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum TaskCommandIntent {
    #[serde(rename_all = "camelCase")]
    CreateChildChat {
        parent_task_id: String,
        request: luna_proto::CreateChildThreadRequest,
        /// Private preflight evidence, just like the agent call's execution
        /// fence. It is not serialized into the user's idempotency payload,
        /// deserialized from clients, or recalculated for an applied receipt.
        #[serde(skip)]
        execution: Option<ChatConfig>,
        /// Engine-set only; absent for legacy payloads (unchanged hash).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access_grant: Option<child_threads::ChildAccessGrant>,
        /// Trusted user tab creation only; absent on legacy/agent commands.
        #[serde(default, skip_serializing_if = "child_threads::not_draft")]
        draft: bool,
    },
    ChildOutcome {
        outcome: luna_proto::ChildThreadOutcome,
    },
    #[serde(rename_all = "camelCase")]
    ChildOutcomeDelivered {
        outcome_id: String,
    },
    #[serde(rename_all = "camelCase")]
    CreateChat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        side_parent_id: Option<String>,
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
    RepairWorkspaceIdentity {
        preview_id: String,
        evidence_digest: String,
    },
    #[serde(rename_all = "camelCase")]
    Session {
        command: SessionCommandPayload,
        #[serde(default)]
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    },
    /// Distinct admission kind makes older execution hosts reject native
    /// review instead of ignoring RunRequest.review and starting a text turn.
    #[serde(rename_all = "camelCase")]
    NativeReview {
        request: luna_proto::RunRequest,
        message_id: String,
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
    SetSettled {
        settled: bool,
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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        side_chat: bool,
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
    #[serde(rename_all = "camelCase")]
    RewindConversation {
        message_id: String,
        /// Explicit conversation-only choice. File restore uses its own preview.
        keep_files: bool,
        review: HistoryReviewToken,
    },
    #[serde(rename_all = "camelCase")]
    ReconcileConversationRewind {
        rewind_command_id: String,
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
    #[serde(rename_all = "camelCase")]
    ApplyContextTransfer {
        request: luna_proto::ContextTransferRequest,
        preview_id: String,
    },
    Goal {
        operation: luna_proto::GoalOperation,
    },
    CompactContext {},
    ClearContext {},
    SwitchProvider {
        config: ChatConfig,
        #[serde(default)]
        handoff: Option<ContextHandoff>,
        /// A direct picker choice must not become a context reset if another
        /// client starts this task while the provider catalog is loading.
        /// Omission preserves existing command payload hashes and behavior.
        #[serde(
            default,
            rename = "initialOnly",
            skip_serializing_if = "Option::is_none"
        )]
        initial_only: Option<bool>,
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
        input_items: Vec<luna_proto::ComposerInput>,
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
        #[serde(default)]
        input_items: Option<Vec<luna_proto::ComposerInput>>,
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
    destination: luna_proto::Space,
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
            Self::CreateChildChat { .. } => "createChildChat",
            Self::ChildOutcome { .. } => "childOutcome",
            Self::ChildOutcomeDelivered { .. } => "childOutcomeDelivered",
            Self::CreateChat { .. } => "createChat",
            Self::RepairWorkspaceIdentity { .. } => "repairWorkspaceIdentity",
            Self::Session { command, .. } => match command {
                SessionCommandPayload::Run { .. } => "run",
                SessionCommandPayload::Steer { .. } => "steer",
                SessionCommandPayload::Interrupt {} => "stop",
                SessionCommandPayload::RespondInput { .. } => "respondInput",
                SessionCommandPayload::RespondAsyncQuestion { .. } => "respondAsyncQuestion",
                SessionCommandPayload::RespondApproval { .. } => "respondApproval",
                SessionCommandPayload::HoldInput { .. } => "holdInput",
            },
            Self::NativeReview { .. } => "nativeReview",
            Self::Rename { .. } => "rename",
            Self::SetPinned { .. } => "setPinned",
            Self::SetRead { .. } => "setRead",
            Self::SetSettled { .. } => "setSettled",
            Self::SetArchived { .. } => "setArchived",
            Self::MoveProject { .. } => "moveProject",
            Self::ForkTask { .. } => "forkTask",
            Self::EditEarlierMessage { .. } => "editEarlierMessage",
            Self::RestoreHistoryRecovery { .. } => "restoreHistoryRecovery",
            Self::RewindConversation { .. } => "rewindConversation",
            Self::ReconcileConversationRewind { .. } => "reconcileConversationRewind",
            Self::Delete {} => "delete",
            Self::SetBranch { .. } => "setBranch",
            Self::SetCwd { .. } => "setCwd",
            Self::SetActivity { .. } => "setActivity",
            Self::SetHost { .. } => "setHost",
            Self::SetConfig { .. } => "setConfig",
            Self::SetModel { .. } => "setModel",
            Self::ApplyContextTransfer { .. } => "applyContextTransfer",
            Self::Goal { .. } => "goal",
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
        matches!(self, Self::Session { command: SessionCommandPayload::Run { request, .. }, .. } if request.review.is_some())
            || matches!(
                self,
                Self::Rename { .. }
                    | Self::RepairWorkspaceIdentity { .. }
                    | Self::SetPinned { .. }
                    | Self::SetRead { .. }
                    | Self::SetSettled { .. }
                    | Self::SetArchived { .. }
                    | Self::MoveProject { .. }
                    | Self::ForkTask { .. }
                    | Self::EditEarlierMessage { .. }
                    | Self::RestoreHistoryRecovery { .. }
                    | Self::RewindConversation { .. }
                    | Self::ReconcileConversationRewind { .. }
                    | Self::Delete {}
                    | Self::SetBranch { .. }
                    | Self::SetCwd { .. }
                    | Self::SetHost { .. }
                    | Self::SetConfig { .. }
                    | Self::SetModel { .. }
                    | Self::ApplyContextTransfer { .. }
                    | Self::Goal { .. }
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
        !matches!(
            self,
            Self::CreateChildChat { .. } | Self::CreateChat { .. } | Self::SetActivity { .. }
        )
    }

    fn conflicts_with_active_turn(&self) -> bool {
        matches!(self, Self::ApplyContextTransfer { request, .. } if request.kind == luna_proto::ContextTransferKind::ProviderHandoff)
            || matches!(self, Self::Session { command: SessionCommandPayload::Run { request, .. }, .. } if request.review.is_some())
            || matches!(
                self,
                Self::SetArchived { .. }
                    | Self::RepairWorkspaceIdentity { .. }
                    | Self::SetSettled { .. }
                    | Self::MoveProject { .. }
                    | Self::ForkTask { .. }
                    | Self::EditEarlierMessage { .. }
                    | Self::RestoreHistoryRecovery { .. }
                    | Self::RewindConversation { .. }
                    | Self::ReconcileConversationRewind { .. }
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
struct SavePromptStashParams {
    entry: luna_proto::PromptStashEntry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeletePromptStashParams {
    id: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum DraftSubmissionOutcome {
    Pending,
    Recoverable,
    Applied,
    Recovered,
    Discarded,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayCommandParams {
    chat_id: String,
    /// The full command entry, client-minted id included — the exactly-once
    /// key the host claims in its processed ledger before executing.
    entry: luna_doc::SessionCommandEntry,
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
    #[serde(default)]
    input_items: Vec<luna_proto::ComposerInput>,
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
    #[serde(default)]
    input_items: Option<Vec<luna_proto::ComposerInput>>,
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
        ToolCall::ViewImage { path } => Some(path),
        ToolCall::ApplyPatch { path } | ToolCall::Search { path, .. } => path.as_deref(),
        ToolCall::Exec { .. }
        | ToolCall::Glob { .. }
        | ToolCall::WebFetch { .. }
        | ToolCall::WebSearch { .. }
        | ToolCall::Todo { .. }
        | ToolCall::Mcp { .. }
        | ToolCall::ComputerUse { .. }
        | ToolCall::Unknown { .. } => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectCommandsParams {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    chat_id: String,
    #[serde(default)]
    action_id: String,
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    run_id: String,
    actions: Option<Vec<luna_proto::ProjectAction>>,
    expected_actions: Option<Vec<luna_proto::ProjectAction>>,
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
    #[serde(default)]
    background: bool,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationUrlParams {
    chat_id: String,
    request_id: String,
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
        /// The project the session is created in; fixes host device + base cwd.
        #[serde(default)]
        space_id: Option<String>,
        /// Legacy host-device hint; the selected project is authoritative.
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
    #[serde(rename_all = "camelCase")]
    SetProjectExecutionDefaults {
        space_id: String,
        defaults: luna_proto::ProjectExecutionDefaults,
        expected_defaults: luna_proto::ProjectExecutionDefaults,
    },
    /// Permanent local deletion; the UI supplies the exact reviewed child set.
    #[serde(rename_all = "camelCase")]
    DeleteSpace {
        space_id: String,
        #[serde(default)]
        expected_chat_ids: Option<Vec<String>>,
    },
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
    /// Full-config replace on the chat row (luna `SetChatConfig`): the
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

mod agent_workspace;
mod automations;
mod board;
mod claude_goals;
mod claude_history;
mod context_transfer;
mod native_extensions;
mod prompt_stash;
mod provider_defaults;
mod side_chats;
mod task_work;

pub struct EngineRpc {
    provider_defaults: Option<crate::provider_defaults::ProviderDefaults>,
    work_writes: Option<crate::device_write::DeviceWriteHandle>,
    chat_workbenches: Option<crate::chat_workbench::ChatWorkbenches>,
    composer_catalog: std::sync::Mutex<crate::composer_catalog::CatalogCache>,
    github_pr: Option<crate::source_control::pull_requests::GitHubPullRequests>,
    side_chats: Option<crate::side_chats::SideChats>,
    backup_control: Option<BackupControl>,
    managed_backup_root: Option<std::path::PathBuf>,
    store: std::sync::Arc<DocsStore>,
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    registry: std::sync::Arc<HarnessRegistry>,
    repos: Repos,
    git_actions: crate::git_actions::GitActions,
    git_checkpoints: Option<crate::git_checkpoints::GitCheckpoints>,
    workspace_files: crate::WorkspaceFiles,
    terminals: Terminals,
    project_commands: Option<crate::project_commands::ProjectCommands>,
    previews: Option<luna_preview::PreviewService>,
    change_requests: CheckoutChangeRequests,
    diff_sync: CheckoutDiffSync,
    uploads: Uploads,
    agent_accounts: AgentAccounts,
    model_cache: std::sync::Arc<crate::model_cache::ModelCache>,
    usage_cache: std::sync::Arc<crate::usage_cache::UsageCache>,
    task_identity: crate::TaskIdentityService,
    auth: Option<Auth>,
    remote_auth: Option<std::sync::Arc<luna_rpc::RemoteAuthStore>>,
    links: Option<std::sync::Arc<LinkCache>>,
    updater: Option<luna_update::Updater>,
    local_import: Option<crate::local_import::LocalImporter>,
    engine_info: EngineInfo,
    cleanup_boot_id: String,
    automation_reconcile_cursor: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Child worktrees provisioned for an in-flight creation command.
    pending_child_worktrees: std::sync::Mutex<
        std::collections::HashMap<String, child_threads::ProvisionedChildWorktree>,
    >,
    activity_cancel: CancellationToken,
    /// Engine-shared board gates and observation notifier.
    board: board_engine::BoardHub,
}

/// A disconnected RPC can abort a native metadata command after its durable
/// receipt is accepted. Keep that receipt explicitly ambiguous instead of
/// leaving an unexplained pending row that appears safe to replay.
struct NativeMetadataCommandGuard<'a> {
    doc_host: &'a DocHost,
    command_id: &'a str,
    armed: bool,
}

impl Drop for NativeMetadataCommandGuard<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.doc_host.mark_task_command_unknown(
                self.command_id,
                "native provider metadata command was interrupted before its final state was confirmed",
            )
        {
            tracing::error!(command_id = %self.command_id, %error, "failed to mark canceled native metadata command unknown");
        }
    }
}

impl Drop for EngineRpc {
    fn drop(&mut self) {
        self.activity_cancel.cancel();
    }
}

impl EngineRpc {
    /// The selected task's current native Claude session id, only when that
    /// task belongs to the selected project (never mix project evidence).
    /// The selected chat's own provider session id, when that chat runs on
    /// `provider` in the selected project (the inventory's session evidence).
    fn provider_session_for_inventory(
        &self,
        provider: HarnessId,
        task_id: Option<&str>,
        project_id: Option<&str>,
    ) -> Option<String> {
        let chat = self.workspace.chat(task_id?).ok().flatten()?;
        if chat.space_id.as_deref() != project_id || chat.config.as_ref()?.harness != provider {
            return None;
        }
        self.task_identity
            .view(&chat.id)
            .and_then(|view| view.current_binding)
            .filter(|binding| binding.provider_id == provider)
            .map(|binding| binding.provider_task_id)
            .or(chat.harness_session_id)
            .filter(|id| !id.is_empty())
    }

    fn skill_roots(
        &self,
        project_id: Option<&str>,
    ) -> Result<crate::integration_settings::SkillRoots, RpcError> {
        let project_root = if let Some(id) = project_id {
            let space = self
                .workspace
                .space(id)
                .map_err(|error| RpcError::Failed(error.to_string()))?
                .ok_or_else(|| RpcError::BadParams("unknown project id".into()))?;
            if space.device_id != self.engine_info.device_id {
                return Err(RpcError::Failed(
                    "Skill library requires the owning device".into(),
                ));
            }
            Some(std::path::PathBuf::from(space.path))
        } else {
            None
        };
        let codex_home = self.agent_accounts.codex_home_for_inventory().ok();
        let (claude_home, opencode_home) = self.agent_accounts.skill_config_dirs();
        Ok(crate::integration_settings::SkillRoots {
            codex_home,
            claude_home,
            opencode_home,
            project_root,
            machine_id: self.engine_info.device_id.clone(),
        })
    }

    fn derived_fork_document(&self, id: &str) -> bool {
        id.split_once("--sub--").is_some_and(|(owner, _)| {
            self.workspace.is_side_chat(owner) || self.store.is_side_chat(owner).unwrap_or(true)
        })
    }

    /// Share one board hub across every RPC service of an engine.
    pub(crate) fn with_board_hub(mut self, hub: board_engine::BoardHub) -> Self {
        self.board = hub;
        self
    }

    pub(crate) fn with_side_chats(mut self, side_chats: crate::side_chats::SideChats) -> Self {
        self.side_chats = Some(side_chats);
        self
    }
    pub(crate) fn with_provider_defaults(
        mut self,
        defaults: crate::provider_defaults::ProviderDefaults,
    ) -> Self {
        self.provider_defaults = Some(defaults);
        self
    }
    fn usage_providers_for(harnesses: &[HarnessId]) -> Vec<luna_proto::UsageProvider> {
        [
            (HarnessId::Codex, luna_proto::UsageProvider::Codex),
            (HarnessId::Opencode, luna_proto::UsageProvider::OpenCodeGo),
            (HarnessId::ClaudeCode, luna_proto::UsageProvider::Claude),
        ]
        .into_iter()
        .filter_map(|(harness, provider)| harnesses.contains(&harness).then_some(provider))
        .collect()
    }

    /// Settings → Agents is authoritative for which providers Usage exposes
    /// on this device, including historical buckets and cached quota rows.
    fn retain_enabled_usage(
        snapshot: &mut luna_proto::UsageSnapshot,
        enabled: &[luna_proto::UsageProvider],
    ) {
        snapshot
            .summary
            .sources
            .retain(|source| enabled.contains(&source.provider));
        snapshot
            .summary
            .buckets
            .retain(|bucket| enabled.contains(&bucket.provider));
        snapshot
            .limits
            .retain(|limit| enabled.contains(&limit.provider));
        snapshot.summary.partial = snapshot
            .summary
            .sources
            .iter()
            .any(|source| source.status == luna_proto::UsageSourceStatus::Partial);
    }

    #[allow(clippy::too_many_arguments)] // engine assembly seam, not a public API
    pub(crate) fn new(
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
        model_cache: std::sync::Arc<crate::model_cache::ModelCache>,
        usage_cache: std::sync::Arc<crate::usage_cache::UsageCache>,
        task_identity: crate::TaskIdentityService,
        workspace_scope: WorkspaceScope,
        cleanup_boot_id: String,
    ) -> Self {
        let git_checkpoints = sessions.git_checkpoints().cloned();
        let git_actions = git_checkpoints
            .as_ref()
            .map(crate::git_checkpoints::GitCheckpoints::actions)
            .unwrap_or_else(|| crate::git_actions::GitActions::new(repos.clone()));
        let engine_info = EngineInfo {
            device_id: doc_host.device_id().to_string(),
            workspace_scope,
            profile_id: Some(workspace.profile_id(workspace_scope)),
            capabilities: luna_proto::capabilities::current()
                .into_iter()
                .filter(|capability| capability != luna_proto::capabilities::CHAT_WORKBENCH_V1)
                .collect(),
        };
        let rpc = Self {
            work_writes: None,
            chat_workbenches: None,
            provider_defaults: None,
            composer_catalog: std::sync::Mutex::new(
                crate::composer_catalog::CatalogCache::default(),
            ),
            github_pr: None,
            side_chats: None,
            backup_control: None,
            managed_backup_root: None,
            store,
            sessions,
            doc_host,
            workspace,
            registry,
            git_actions,
            git_checkpoints,
            repos,
            workspace_files,
            terminals,
            project_commands: None,
            previews: None,
            change_requests,
            diff_sync,
            uploads,
            agent_accounts,
            model_cache,
            usage_cache,
            task_identity,
            auth: None,
            remote_auth: None,
            links: None,
            updater: None,
            local_import: None,
            engine_info,
            cleanup_boot_id,
            automation_reconcile_cursor: std::sync::Arc::new(std::sync::Mutex::new(None)),
            pending_child_worktrees: Default::default(),
            activity_cancel: CancellationToken::new(),
            board: board_engine::BoardHub::default(),
        };
        rpc.spawn_activity_bootstrap();
        rpc.model_cache.start_background_refresh();
        rpc.usage_cache.start();
        rpc
    }

    pub(crate) fn with_work_write_handle(
        mut self,
        writes: crate::device_write::DeviceWriteHandle,
    ) -> Self {
        self.work_writes = Some(writes);
        self
    }

    pub(crate) fn with_project_commands(
        mut self,
        commands: crate::project_commands::ProjectCommands,
    ) -> Self {
        self.project_commands = Some(commands);
        self
    }

    pub(crate) fn resume_cleanup_initial(&self) -> (String, String) {
        let (task_after, _) = self.resume_pending_task_cleanups_after("");
        let (project_after, _) = self.resume_pending_project_cleanups_after("");
        (task_after, project_after)
    }

    /// One keyset page. Only a committed receipt/identity tombstone authorizes
    /// cleanup; a different boot's preparation with a surviving chat is
    /// abandoned and its draft becomes writable again. Same-boot pending
    /// records may belong to another RPC service executing right now.
    fn resume_pending_task_cleanups_after(&self, after: &str) -> (String, usize) {
        let tasks = match self.store.pending_task_cleanups_after(after, 16) {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::warn!(%error, "task cleanup resume scan failed");
                return (after.to_string(), 0);
            }
        };
        let count = tasks.len();
        let next = tasks
            .last()
            .map(|(id, _, _)| id.clone())
            .unwrap_or_default();
        for (task_id, state, owner_boot_id) in tasks {
            if self.sessions.has_live_run(&task_id)
                || (state == "pending" && owner_boot_id == self.cleanup_boot_id)
            {
                continue;
            }
            let Some(_gate) = self.doc_host.try_task_command_gate(&task_id) else {
                continue;
            };
            let tombstoned = self
                .task_identity
                .view(&task_id)
                .is_some_and(|view| view.task.tombstoned);
            let committed_delete = match self.store.has_applied_task_delete(&task_id) {
                Ok(committed) => committed,
                Err(error) => {
                    tracing::warn!(task = %task_id, %error, "delete receipt read failed");
                    continue;
                }
            };
            let chat = match self.workspace.chat(&task_id) {
                Ok(chat) => chat,
                Err(error) => {
                    tracing::warn!(task = %task_id, %error, "cleanup registry read failed");
                    continue;
                }
            };
            if chat.is_some() {
                if !tombstoned
                    && !committed_delete
                    && let Err(error) = self.store.abort_task_cleanup(&task_id)
                {
                    tracing::warn!(task = %task_id, %error, "abandoned cleanup could not be cleared");
                }
                continue;
            }
            if tombstoned || committed_delete {
                self.cleanup_deleted_task(
                    &task_id,
                    self.task_identity
                        .view(&task_id)
                        .is_some_and(|view| view.current_binding.is_some()),
                );
            }
        }
        (next, count)
    }

    fn cleanup_deleted_task(&self, task_id: &str, provider_bound: bool) {
        let result = (|| -> Result<(), String> {
            self.doc_host.evict_chat(task_id);
            if let Some(service) = &self.side_chats {
                service.retire(task_id).map_err(|error| error.to_string())?;
            }
            self.store
                .retire_task_local_records(&self.engine_info.device_id, task_id)
                .map_err(|error| error.to_string())?;
            self.terminals
                .delete_task(task_id)
                .map_err(|error| error.to_string())?;
            for path in self
                .store
                .task_cleanup_paths(task_id)
                .map_err(|error| error.to_string())?
            {
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                if !metadata.file_type().is_file() || self.uploads.owned_blob_path(&path).is_none()
                {
                    return Err(format!(
                        "draft blob is outside the verified LUNA upload root: {path}"
                    ));
                }
                self.store
                    .remove_unowned_attachment(&path)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })();
        let (state, detail) = match result {
            Ok(()) if provider_bound => ("provider-pending", "LUNA data was removed. Provider-native data remains; automatic provider deletion is unsupported.".to_string()),
            Ok(()) => ("completed", "LUNA-owned local cleanup completed.".to_string()),
            Err(error) => ("failed", format!("Local cleanup failed: {error}")),
        };
        if let Err(error) = self.store.set_task_cleanup_state(task_id, state, &detail) {
            tracing::warn!(task = %task_id, %error, "task cleanup state could not be persisted");
        }
        if state == "failed" {
            tracing::warn!(task = %task_id, %detail, "task cleanup incomplete");
        }
    }

    fn finish_project_cleanup(&self, project_id: &str, children: &[String]) {
        let result = (|| -> Result<bool, String> {
            self.store
                .retire_project_local_records(project_id)
                .map_err(|error| error.to_string())?;
            let mut provider_pending = false;
            for task_id in children {
                match self
                    .store
                    .task_cleanup_state(task_id)
                    .map_err(|error| error.to_string())?
                {
                    Some((state, _)) if state == "completed" => {}
                    Some((state, _)) if state == "provider-pending" => provider_pending = true,
                    Some((_, detail)) => return Err(format!("task {task_id}: {detail}")),
                    None => return Err(format!("task {task_id} cleanup status is missing")),
                }
            }
            Ok(provider_pending)
        })();
        let (state, detail) = match result {
            Ok(true) => (
                "provider-pending",
                "LUNA project cleanup completed; provider-native cleanup is unsupported."
                    .to_string(),
            ),
            Ok(false) => (
                "completed",
                "LUNA-owned project cleanup completed.".to_string(),
            ),
            Err(error) => ("failed", format!("Project cleanup failed: {error}")),
        };
        if let Err(error) = self
            .store
            .set_project_cleanup_state(project_id, state, &detail)
        {
            tracing::warn!(project = %project_id, %error, "project cleanup state could not be persisted");
        }
    }

    fn release_surviving_project_children(&self, children: &[String]) {
        for task_id in children {
            if matches!(self.workspace.chat(task_id), Ok(Some(_)))
                && !self
                    .task_identity
                    .view(task_id)
                    .is_some_and(|view| view.task.tombstoned)
                && matches!(self.store.has_applied_task_delete(task_id), Ok(false))
                && let Err(error) = self.store.abort_task_cleanup(task_id)
            {
                tracing::warn!(task = %task_id, %error, "surviving task cleanup marker could not be cleared");
            }
        }
    }

    fn resume_pending_project_cleanups_after(&self, after: &str) -> (String, usize) {
        self.resume_pending_project_cleanups_with_readiness(after, self.workspace.registry_synced())
    }

    #[doc(hidden)]
    pub fn run_project_cleanup_maintenance_for_tests(&self) {
        self.resume_pending_project_cleanups_after("");
    }

    fn resume_pending_project_cleanups_with_readiness(
        &self,
        after: &str,
        authoritative_registry: bool,
    ) -> (String, usize) {
        if !authoritative_registry {
            return (after.to_string(), 0);
        }
        let projects = match self.store.pending_project_cleanups_after(after, 8) {
            Ok(projects) => projects,
            Err(error) => {
                tracing::warn!(%error, "project cleanup resume scan failed");
                return (after.to_string(), 0);
            }
        };
        let count = projects.len();
        let next = projects
            .last()
            .map(|(id, _, _, _)| id.clone())
            .unwrap_or_default();
        for (project_id, children, state, owner_boot_id) in projects {
            if state == "pending" && owner_boot_id == self.cleanup_boot_id {
                continue; // the initiating service may still be committing
            }
            let mut gate_ids = children.clone();
            gate_ids.sort();
            gate_ids.dedup();
            let mut _gates = Vec::with_capacity(gate_ids.len());
            let mut busy = false;
            for task_id in gate_ids {
                match self.doc_host.try_task_command_gate(&task_id) {
                    Some(gate) => _gates.push(gate),
                    None => {
                        busy = true;
                        break;
                    }
                }
            }
            if busy {
                continue;
            }
            let project = match self.workspace.space(&project_id) {
                Ok(project) => project,
                Err(error) => {
                    let _ = self.store.set_project_cleanup_state(
                        &project_id,
                        "failed",
                        &format!("Registry read failed: {error}"),
                    );
                    continue;
                }
            };
            if project.is_some() {
                // An old boot prepared a deletion that never committed. Clear
                // its markers only when every child still exists and no child
                // has committed deletion evidence. Mixed state needs review.
                if owner_boot_id != self.cleanup_boot_id {
                    let intact = children.iter().all(|task_id| {
                        self.workspace.chat(task_id).ok().flatten().is_some()
                            && !self
                                .task_identity
                                .view(task_id)
                                .is_some_and(|view| view.task.tombstoned)
                            && matches!(self.store.has_applied_task_delete(task_id), Ok(false))
                    });
                    if intact {
                        for task_id in &children {
                            let _ = self.store.abort_task_cleanup(task_id);
                        }
                        let _ = self.store.abort_project_cleanup(&project_id);
                    } else {
                        let _ = self.store.set_project_cleanup_state(&project_id, "failed",
                            "Project still exists but child state changed; cleanup was not resumed.");
                    }
                }
                continue;
            }
            // An absent project row is insufficient evidence by itself. Check
            // EVERY reviewed child before touching ANY child: a moved task is
            // still live data even when its former project was later removed.
            let mut failure = None;
            for task_id in &children {
                if self.sessions.has_live_run(task_id) {
                    failure = Some(format!("Task {task_id} is still executing"));
                    break;
                }
                match self.workspace.chat(task_id) {
                    Ok(None) => {}
                    Ok(Some(chat)) => {
                        failure = Some(format!(
                            "Task {task_id} still exists in project {:?}",
                            chat.space_id
                        ));
                        break;
                    }
                    Err(error) => {
                        failure = Some(format!("Task {task_id} registry read failed: {error}"));
                        break;
                    }
                }
            }
            if let Some(error) = failure {
                let _ = self
                    .store
                    .set_project_cleanup_state(&project_id, "failed", &error);
                self.release_surviving_project_children(&children);
                continue;
            }
            // The previous process may have removed these rows only in memory
            // before its checked snapshot save failed. Persist the validated
            // deletion now, while the child command gates are held, before
            // retiring identity, snapshots, drafts, terminals or upload bytes.
            if let Err(error) = self.workspace.flush_checked() {
                let _ = self.store.set_project_cleanup_state(
                    &project_id,
                    "failed",
                    &format!("Registry deletion is not durable: {error}"),
                );
                continue;
            }
            let persisted_selection = self.workspace.registry_synced()
                && matches!(self.workspace.space(&project_id), Ok(None))
                && children
                    .iter()
                    .all(|task_id| matches!(self.workspace.chat(task_id), Ok(None)));
            if !persisted_selection {
                let detail = "Project or child state changed after the checked registry flush; cleanup was not resumed.";
                let _ = self
                    .store
                    .set_project_cleanup_state(&project_id, "failed", detail);
                self.release_surviving_project_children(&children);
                continue;
            }
            for task_id in &children {
                // Recheck immediately before each irreversible child step.
                if !matches!(self.workspace.chat(task_id), Ok(None)) {
                    failure = Some(format!("Task {task_id} changed during cleanup"));
                    break;
                }
                if let Some(view) = self.task_identity.view(task_id)
                    && !view.task.tombstoned
                    && let Err(error) = self.task_identity.tombstone_task(task_id)
                {
                    failure = Some(error.to_string());
                    break;
                }
                if let Err(error) = self.store.prepare_task_cleanup(
                    &self.engine_info.device_id,
                    task_id,
                    &self.cleanup_boot_id,
                ) {
                    failure = Some(error.to_string());
                    break;
                }
                let provider_bound = self
                    .task_identity
                    .view(task_id)
                    .is_some_and(|view| view.current_binding.is_some());
                self.cleanup_deleted_task(task_id, provider_bound);
            }
            if let Some(error) = failure {
                let _ = self
                    .store
                    .set_project_cleanup_state(&project_id, "failed", &error);
                self.release_surviving_project_children(&children);
            } else {
                self.finish_project_cleanup(&project_id, &children);
            }
        }
        (next, count)
    }

    /// One worker per EngineCore, retained and cancelled by that owner. Full
    /// pages continue after a yield; idle/blocked work waits for the next
    /// thirty-second tick. The keyset cursor prevents a failed first row from
    /// starving later rows, and every destructive step rechecks authority.
    pub(crate) fn start_cleanup_maintenance(
        self: &std::sync::Arc<Self>,
        cancel: CancellationToken,
        mut task_after: String,
        mut project_after: String,
    ) -> tokio::task::JoinHandle<()> {
        let weak = std::sync::Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {},
                }
                if let Some(rpc) = weak.upgrade() {
                    rpc.retire_unsupported_history().await;
                }
                let mut task_done = false;
                let mut project_done = false;
                loop {
                    let Some(rpc) = weak.upgrade() else { return };
                    if !task_done {
                        let (next, count) = rpc.resume_pending_task_cleanups_after(&task_after);
                        task_after = next;
                        task_done = count < 16;
                    }
                    if !project_done {
                        let (next, count) =
                            rpc.resume_pending_project_cleanups_after(&project_after);
                        project_after = next;
                        project_done = count < 8;
                    }
                    drop(rpc);
                    if task_done && project_done {
                        break;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
                    }
                }
            }
        })
    }

    async fn retire_unsupported_history(&self) {
        let chats = match self.workspace.read_chats() {
            Ok(chats) => chats,
            Err(error) => {
                tracing::warn!(%error, "retired provider history scan failed");
                return;
            }
        };
        for chat in chats
            .into_iter()
            .filter(|chat| {
                chat.device_id == self.engine_info.device_id
                    && (chat
                        .config
                        .as_ref()
                        .is_some_and(|config| config.harness.is_retired())
                        || self.task_identity.view(&chat.id).is_some_and(|view| {
                            view.current_binding
                                .as_ref()
                                .is_some_and(|binding| binding.provider_id.is_retired())
                        }))
            })
            .take(16)
        {
            let command = legacy_task_command(chat.id.clone(), TaskCommandIntent::Delete {});
            match self.execute_task_command(command, true).await {
                Ok(receipt) if receipt.status == luna_proto::CommandReceiptStatus::Applied => {
                    tracing::info!(task = %chat.id, "retired provider task removed from LUNA history");
                }
                Ok(receipt) => {
                    tracing::warn!(task = %chat.id, status = ?receipt.status, "retired provider task cleanup deferred")
                }
                Err(error) => {
                    tracing::warn!(task = %chat.id, %error, "retired provider task cleanup failed")
                }
            }
        }
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

    pub(crate) fn with_github_pr(
        mut self,
        service: crate::source_control::pull_requests::GitHubPullRequests,
    ) -> Self {
        self.github_pr = Some(service);
        self
    }

    pub fn with_previews(mut self, previews: luna_preview::PreviewService) -> Self {
        self.previews = Some(previews);
        self
    }

    /// Attach the auth service (AuthStatus + AuthRpc mutations).
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn with_remote_auth(mut self, auth: std::sync::Arc<luna_rpc::RemoteAuthStore>) -> Self {
        self.remote_auth = Some(auth);
        self.engine_info
            .capabilities
            .push(luna_proto::capabilities::REMOTE_AUTH_V1.into());
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

    pub fn with_managed_backup_root(mut self, root: std::path::PathBuf) -> Self {
        self.managed_backup_root = Some(root);
        self
    }

    /// Attach the peer link cache — enables `targetDeviceId` relay forwarding.
    pub fn with_links(mut self, links: std::sync::Arc<LinkCache>) -> Self {
        self.links = Some(links);
        self
    }

    /// Attach the release checker (UpdateStatus stream + ApplyUpdate).
    pub fn with_updater(mut self, updater: luna_update::Updater) -> Self {
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

    fn updater(&self) -> Result<&luna_update::Updater, RpcError> {
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
            // The projector only uses the newest assistant. Polling Activity
            // must not deep-read every settled message in every local chat.
            let latest_assistant = handle
                .doc()
                .read_latest_assistant_entry()
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
                latest_assistant.as_slice(),
                &queue,
            ));
        }
        for source in self
            .store
            .automation_attention_sources(&self.engine_info.device_id)
            .map_err(|e| RpcError::Failed(e.to_string()))?
        {
            if let Some(item) =
                crate::attention::project_automation(&self.engine_info.device_id, &source)
            {
                desired.push(item);
            }
        }
        self.store
            .reconcile_attention_items(&desired, crate::attention::MANAGED_KINDS)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let stored = self
            .store
            .attention_snapshot()
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        Ok(AttentionSnapshot {
            contract_version: luna_proto::ATTENTION_CONTRACT_VERSION,
            revision: stored.revision,
            read_at_ms: crate::now_ms(),
            unresolved_count: stored.unresolved_count,
            needs_attention: stored.needs_attention,
            recent: stored.recent,
            producers: vec![
                AttentionProducerAvailability { source: "local task requests, run/recovery state, queue, and edge connectivity".into(), available: true, reason: None },
                AttentionProducerAvailability { source: "remote host and device grants".into(), available: false, reason: Some("Scoped remote authorization is not implemented.".into()) },
                AttentionProducerAvailability { source: "automations".into(), available: true, reason: None },
                AttentionProducerAvailability { source: "engine/CLI compatibility blockers".into(), available: false, reason: Some("A stable operation-blocking compatibility producer is not available.".into()) },
                AttentionProducerAvailability { source: "provider account health".into(), available: false, reason: Some("Only a task's authoritative recovery sign-in state is available; no provider health probe is performed.".into()) },
            ],
        })
    }

    fn validate_composer_draft(&self, draft: &ComposerDraft) -> Result<(), RpcError> {
        if draft.version != luna_proto::COMPOSER_DRAFT_VERSION {
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
        let mut capture_ids = HashSet::new();
        let mut screenshot_ids = HashSet::new();
        for shot in &draft.appshots {
            if shot.id.is_empty()
                || shot.id.len() > 128
                || shot.screenshot_attachment_id.is_empty()
                || shot.screenshot_attachment_id.len() > 128
                || !capture_ids.insert(shot.id.as_str())
                || !screenshot_ids.insert(shot.screenshot_attachment_id.as_str())
                || draft
                    .attachments
                    .iter()
                    .filter(|blob| blob.attachment_id == shot.screenshot_attachment_id)
                    .count()
                    != 1
            {
                return Err(RpcError::BadParams(
                    "Appshot screenshot and context must have one matching attachment".into(),
                ));
            }
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

    fn refresh_appshot_history_for_task(&self, task_id: &str) -> Result<(), RpcError> {
        let Some(chat) = self
            .workspace
            .read_chats()
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .into_iter()
            .find(|chat| chat.id == task_id)
        else {
            // A newly minted canvas task may not have published its chat row
            // yet. Its canonical Appshot facts remain for the next task index.
            return Ok(());
        };
        let _activity_guard = self.store.activity_index_guard();
        self.store
            .bind_appshot_target_environment(task_id, &chat.device_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let policy = chat
            .space_id
            .as_deref()
            .map(|project_id| self.store.activity_project_policy(project_id))
            .transpose()
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .flatten();
        let records = self
            .store
            .appshot_activity_for_task(task_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let identity = self.task_identity.view(task_id);
        let events = crate::activity_history::build_appshot_envelopes(
            &self.engine_info.device_id,
            &chat,
            identity.as_ref(),
            &records,
            None,
            policy.as_ref(),
        );
        self.store
            .replace_appshot_activity_events(task_id, &events)
            .map_err(|error| RpcError::Failed(error.to_string()))
    }

    /// Resolve a mention-search root from synced workspace rows. A client may
    /// name an existing linked worktree for a new chat, but it is verified
    /// against the space repository before any filesystem walk begins.
    async fn file_search_root(&self, p: &FileSearchParams) -> Result<std::path::PathBuf, RpcError> {
        let target = luna_proto::WorkspaceTarget {
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

    /// Git card requests must retain the checkout owned by the selected task.
    /// A fresh `git rev-parse` alone is not provenance: when a nested
    /// worktree's .git disappears, Git resolves its ancestor repository.
    async fn trusted_git_checkout(
        &self,
        chat_id: &str,
        cwd: &str,
        expected_checkout_id: Option<&str>,
    ) -> Result<String, RpcError> {
        let chat = self
            .workspace
            .chat(chat_id)
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .ok_or_else(|| RpcError::Failed("Git task is unavailable".into()))?;
        if chat.device_id != self.doc_host.device_id() || chat.cwd.as_deref() != Some(cwd) {
            return Err(RpcError::Failed(
                "Task working directory changed; refresh Git".into(),
            ));
        }
        let recorded = chat.checkout_id.ok_or_else(|| {
            RpcError::Failed(
                "Task checkout identity is unavailable; reopen the task after workspace discovery"
                    .into(),
            )
        })?;
        if expected_checkout_id.is_some_and(|expected| expected != recorded.as_str()) {
            return Err(RpcError::Failed(
                "Task checkout changed; refresh Git".into(),
            ));
        }
        let current = self
            .repos
            .checkout_identity(std::path::Path::new(cwd))
            .await
            .map_err(|_| RpcError::Failed("Git checkout is unavailable".into()))?;
        if current.id != recorded {
            return Err(RpcError::Failed(
                "Task checkout changed; restore its worktree or create a new task in the intended checkout"
                    .into(),
            ));
        }
        Ok(recorded)
    }

    fn git_request_chat_id(params: &serde_json::Value) -> Result<&str, RpcError> {
        params
            .get("chatId")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| RpcError::BadParams("chatId is required for Git actions".into()))
    }

    fn checkpoint_service(&self) -> Result<&crate::git_checkpoints::GitCheckpoints, RpcError> {
        self.git_checkpoints.as_ref().ok_or_else(|| {
            RpcError::Failed("Git checkpoints are unavailable in this engine".into())
        })
    }

    fn validate_checkpoint_task(&self, chat_id: &str, cwd: &str) -> Result<(), RpcError> {
        let chat = self
            .workspace
            .chat(chat_id)
            .map_err(|_| RpcError::Failed("Task registry is unavailable".into()))?;
        if chat.is_some_and(|chat| {
            chat.device_id == self.doc_host.device_id() && chat.cwd.as_deref() == Some(cwd)
        }) {
            Ok(())
        } else {
            Err(RpcError::BadParams(
                "Checkpoint task and local checkout do not match".into(),
            ))
        }
    }

    /// Fail closed if the target itself moved/disappeared or another local
    /// task owns this worktree or an enclosing directory.
    fn checkpoint_owner_overlap_now(&self, chat_id: &str, root: &std::path::Path) -> bool {
        let Ok(chats) = self.workspace.read_chats() else {
            return true;
        };
        let Some(target) = chats.iter().find(|chat| chat.id == chat_id) else {
            return true;
        };
        if target.device_id != self.doc_host.device_id()
            || target
                .cwd
                .as_deref()
                .and_then(|cwd| std::path::Path::new(cwd).canonicalize().ok())
                .as_deref()
                != Some(root)
            || self.sessions.turn_in_flight(chat_id)
        {
            return true;
        }
        if self.task_identity.view(chat_id).is_some_and(|view| {
            view.task.tombstoned
                || std::path::Path::new(&view.workspace.current_path)
                    .canonicalize()
                    .ok()
                    .as_deref()
                    != Some(root)
        }) {
            return true;
        }
        for chat in chats
            .iter()
            .filter(|chat| chat.device_id == self.doc_host.device_id())
        {
            let Some(other_cwd) = chat.cwd.as_deref() else {
                continue;
            };
            if chat.id == chat_id {
                continue;
            }
            let Ok(other_path) = std::path::Path::new(other_cwd).canonicalize() else {
                continue;
            };
            if restore_owner_paths_overlap(root, &other_path) {
                return true;
            }
        }
        false
    }

    async fn checkpoint_checkout_busy(&self, chat_id: &str, cwd: &str) -> Result<bool, RpcError> {
        let identity = self
            .repos
            .checkout_identity(std::path::Path::new(cwd))
            .await
            .map_err(|_| RpcError::Failed("Git checkout is unavailable".into()))?;
        Ok(self.checkpoint_owner_overlap_now(chat_id, &identity.root))
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
            MutateParams::SetProjectExecutionDefaults {
                space_id,
                defaults,
                expected_defaults,
            } => self
                .workspace
                .set_project_execution_defaults(
                    &space_id,
                    &self.engine_info.device_id,
                    &defaults,
                    &expected_defaults,
                )
                .map_err(failed),
            MutateParams::DeleteSpace {
                space_id,
                expected_chat_ids,
            } => {
                let mut expected = expected_chat_ids.ok_or_else(|| {
                    RpcError::BadParams("project deletion requires a reviewed session list".into())
                })?;
                expected.sort();
                expected.dedup();
                let space = self
                    .workspace
                    .space(&space_id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::BadParams("project no longer exists".into()))?;
                if space.device_id != self.engine_info.device_id {
                    return Err(RpcError::Failed(
                        "remote project cleanup is unsupported on this device".into(),
                    ));
                }
                let mut current: Vec<_> = self
                    .workspace
                    .read_chats()
                    .map_err(failed)?
                    .into_iter()
                    .filter(|chat| chat.space_id.as_deref() == Some(&space_id))
                    .collect();
                current.sort_by(|a, b| a.id.cmp(&b.id));
                if current.iter().map(|chat| &chat.id).ne(expected.iter()) {
                    return Err(RpcError::Failed(
                        "project sessions changed; reopen the deletion preview".into(),
                    ));
                }
                if current
                    .iter()
                    .any(|chat| chat.device_id != self.engine_info.device_id)
                {
                    return Err(RpcError::Failed(
                        "project contains remote tasks; remote cleanup is unsupported".into(),
                    ));
                }
                let mut _gates = Vec::with_capacity(expected.len());
                for task_id in &expected {
                    _gates.push(self.doc_host.task_command_gate(task_id).await);
                }
                for task_id in &expected {
                    self.sessions.interrupt(task_id).await.map_err(failed)?;
                    if self.sessions.has_live_run(task_id) {
                        return Err(RpcError::Failed(format!(
                            "task {task_id} did not settle; project was not deleted"
                        )));
                    }
                }
                self.store
                    .prepare_project_cleanup(&space_id, &expected, &self.cleanup_boot_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let mut prepared = Vec::new();
                for task_id in &expected {
                    if let Err(error) = self.store.prepare_task_cleanup(
                        &self.engine_info.device_id,
                        task_id,
                        &self.cleanup_boot_id,
                    ) {
                        for prepared_id in prepared {
                            let _ = self.store.abort_task_cleanup(prepared_id);
                        }
                        let _ = self.store.abort_project_cleanup(&space_id);
                        return Err(RpcError::Failed(error.to_string()));
                    }
                    prepared.push(task_id.as_str());
                }
                let deleted = match self.workspace.delete_space_if_chats(&space_id, &expected) {
                    Ok(deleted) => deleted,
                    Err(error) => {
                        if self.workspace.space(&space_id).ok().flatten().is_some() {
                            for task_id in &expected {
                                let _ = self.store.abort_task_cleanup(task_id);
                            }
                            let _ = self.store.abort_project_cleanup(&space_id);
                        }
                        return Err(failed(error));
                    }
                };
                if let Err(error) = self.workspace.flush_checked() {
                    let _ = self.store.set_project_cleanup_state(
                        &space_id,
                        "failed",
                        &format!("registry deletion could not be durably flushed: {error}"),
                    );
                    return Err(failed(error));
                }
                let registry_safe = matches!(self.workspace.space(&space_id), Ok(None))
                    && deleted
                        .chat_ids
                        .iter()
                        .all(|task_id| matches!(self.workspace.chat(task_id), Ok(None)));
                if !registry_safe {
                    let detail = "Project or one of its tasks changed after the registry flush; local cleanup is pending review.";
                    let _ = self
                        .store
                        .set_project_cleanup_state(&space_id, "failed", detail);
                    self.release_surviving_project_children(&deleted.chat_ids);
                    return Err(RpcError::Failed(detail.into()));
                }
                for task_id in &deleted.chat_ids {
                    let provider_bound = self
                        .task_identity
                        .view(task_id)
                        .is_some_and(|view| view.current_binding.is_some());
                    if self.task_identity.view(task_id).is_some()
                        && let Err(error) = self.task_identity.tombstone_task(task_id)
                    {
                        let _ = self.store.set_project_cleanup_state(
                            &space_id,
                            "failed",
                            &format!("task {task_id} identity cleanup failed: {error}"),
                        );
                        return Err(failed(error));
                    }
                    self.cleanup_deleted_task(task_id, provider_bound);
                }
                self.finish_project_cleanup(&space_id, &deleted.chat_ids);
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
        command: TaskCommandParams,
        legacy: bool,
    ) -> Result<CommandReceipt, RpcError> {
        self.execute_task_command_with_send_fence(command, legacy, None)
            .await
    }

    async fn execute_task_command_with_send_fence(
        &self,
        command: TaskCommandParams,
        legacy: bool,
        send_fence: Option<&crate::cross_task_tools::ApprovedSendFence>,
    ) -> Result<CommandReceipt, RpcError> {
        self.execute_task_command_with_fences(command, legacy, send_fence, None)
            .await
    }

    async fn execute_task_command_with_fences(
        &self,
        mut command: TaskCommandParams,
        legacy: bool,
        send_fence: Option<&crate::cross_task_tools::ApprovedSendFence>,
        automation_fence: Option<&luna_proto::AutomationRecord>,
    ) -> Result<CommandReceipt, RpcError> {
        if self.derived_fork_document(&command.task_id) {
            return Err(RpcError::Failed(
                "Native-agent transcripts are derived documents, not writable threads".into(),
            ));
        }
        if let TaskCommandIntent::NativeReview {
            request,
            message_id,
        } = &command.intent
        {
            if request.review.is_none() {
                return Err(RpcError::Failed("Native review target is missing".into()));
            }
            command.intent = TaskCommandIntent::Session {
                command: SessionCommandPayload::Run {
                    request: request.clone(),
                    message_id: message_id.clone(),
                },
                transfers: Vec::new(),
            };
        }
        let _lane = self.doc_host.task_command_gate(&command.task_id).await;
        let current = self.current_task_command_snapshot(&command.task_id)?;
        if let Some(automation) = automation_fence {
            self.check_automation_dispatch_fence(automation, &command.task_id)?;
        }
        if send_fence.is_some_and(|fence| {
            current.task_revision != fence.target_snapshot.task_revision
                || current.binding_id != fence.target_snapshot.binding_id
                || current.binding_generation != fence.target_snapshot.binding_generation
                || current.active_turn_id != fence.target_snapshot.active_turn_id
        }) {
            return Err(RpcError::Failed(
                "target task changed before command admission".into(),
            ));
        }
        if let Some(fence) = send_fence {
            self.sessions
                .with_live_approved_send(fence, || Ok(()))
                .map_err(|error| RpcError::Failed(error.to_string()))?;
        }
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
            luna_sync::TaskCommandBeginKind::Duplicate => {
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
            luna_sync::TaskCommandBeginKind::Conflict => {
                self.doc_host.publish_task_receipt(begin.receipt.clone());
                return Ok(begin.receipt);
            }
            luna_sync::TaskCommandBeginKind::Accepted => {
                self.doc_host.publish_task_receipt(begin.receipt.clone());
            }
        }

        if !matches!(&command.intent, TaskCommandIntent::CreateChat { .. })
            && self
                .doc_host
                .context_control(&command.task_id)
                .map_err(|error| RpcError::Failed(error.to_string()))?
                .pending_rewind
                .is_some()
            && !matches!(
                &command.intent,
                TaskCommandIntent::ReconcileConversationRewind { .. }
                    | TaskCommandIntent::MarkSeen { .. }
                    | TaskCommandIntent::SetRead { .. }
            )
        {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::OperationFailed,
                "Conversation rewind must be reconciled before another task mutation",
                "Use Reconcile rewind; it reads provider history without repeating the revert.",
            );
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
        // CC-14/15, OC-15: refuse what Claude or OpenCode cannot run here,
        // never queue it: a Claude goal sent mid-turn (Claude would run it
        // later, unannounced) and a review target that is not a plain git ref
        // for its `/code-review` or `/review` line.
        if let TaskCommandIntent::Session {
            command: SessionCommandPayload::Run { request, .. },
            ..
        } = &command.intent
            && (request.goal.is_some() || request.review.is_some())
            && let Some(harness) = self
                .workspace
                .chat(&command.task_id)
                .ok()
                .flatten()
                .and_then(|chat| chat.config)
                .map(|config| config.harness)
                .filter(|harness| matches!(harness, HarnessId::ClaudeCode | HarnessId::Opencode))
        {
            let refusal = if harness == HarnessId::ClaudeCode
                && request.goal.is_some()
                && self.sessions.turn_in_flight(&command.task_id)
            {
                Some((
                    CommandReasonCode::ActiveTurnConflict,
                    "Claude starts a goal between turns. Wait for the current turn to finish, or stop it, then send the goal.".to_owned(),
                    "Send the goal again once Claude is idle.",
                ))
            } else if let Some(review) = request.review.as_ref()
                && let Some(Err(error)) =
                    crate::claude_ongoing::command_line_review(harness, &review.target)
            {
                Some((
                    CommandReasonCode::UnsupportedOperation,
                    error.to_string(),
                    "Choose a plain branch or commit name, then start the review again.",
                ))
            } else {
                None
            };
            if let Some((code, message, recovery)) = refusal {
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Rejected,
                    false,
                    code,
                    &message,
                    recovery,
                );
            }
        }
        if matches!(command.intent, TaskCommandIntent::Delete {}) {
            if let Err(error) = self.sessions.interrupt(&command.task_id).await {
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Failed,
                    false,
                    CommandReasonCode::OperationFailed,
                    &error.to_string(),
                    "The task is still present. Retry deletion after execution stops.",
                );
            }
            if self.sessions.has_live_run(&command.task_id) {
                return self.finish_task_command(
                    &command.command_id,
                    CommandReceiptStatus::Conflict,
                    false,
                    CommandReasonCode::ActiveTurnConflict,
                    "execution did not settle before deletion",
                    "Stop the task and retry.",
                );
            }
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
        if send_fence.is_some_and(|fence| {
            revalidated.task_revision != fence.target_snapshot.task_revision
                || revalidated.binding_id != fence.target_snapshot.binding_id
                || revalidated.binding_generation != fence.target_snapshot.binding_generation
                || revalidated.active_turn_id != fence.target_snapshot.active_turn_id
        }) {
            return self.finish_task_command(
                &command.command_id,
                CommandReceiptStatus::Conflict,
                false,
                CommandReasonCode::StaleTaskRevision,
                "the target changed after approval",
                "Read the task again and request a new approval.",
            );
        }
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
                || !matches!(item.status, luna_doc::QueueMessageStatus::Held)
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
            if (!item.attachments.is_empty()
                && !crate::provider_inputs::steers_native_inputs(harness))
                || !self.sessions.steers_mid_turn(harness)
            {
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

        let native_metadata_effect = self
            .task_identity
            .has_native_codex_binding(&command.task_id)
            && matches!(
                command.intent,
                TaskCommandIntent::Rename { .. } | TaskCommandIntent::SetArchived { .. }
            );
        let provider_effect = native_metadata_effect
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
        let mut cancellation_guard = native_metadata_effect.then(|| NativeMetadataCommandGuard {
            doc_host: &self.doc_host,
            command_id: &command.command_id,
            armed: true,
        });
        // Keep this large command state machine out of every enclosing RPC
        // future. Inline storage compounds debug poll-stack usage even for a
        // small command such as goal resume; the command lane and cancellation
        // guard still surround the same awaited operation.
        let outcome = Box::pin(self.apply_task_command_intent(
            &command.command_id,
            &command.task_id,
            command.intent,
            expected_turn_id.as_deref(),
            validated_project_move,
            send_fence,
        ))
        .await;
        if let Some(guard) = &mut cancellation_guard {
            guard.armed = false;
        }
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
            | TaskCommandIntent::NativeReview { .. }
            | TaskCommandIntent::ForkTask { .. }
            | TaskCommandIntent::EditEarlierMessage { .. }
            | TaskCommandIntent::RestoreHistoryRecovery { .. }
            | TaskCommandIntent::RewindConversation { .. }
            | TaskCommandIntent::ReconcileConversationRewind { .. }
            | TaskCommandIntent::SendQueuedNow { .. }
            | TaskCommandIntent::SteerQueuedNow { .. }
            | TaskCommandIntent::RetryRecovery { .. }
            | TaskCommandIntent::Goal { .. }
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
                "an existing task cannot move to another machine or execution environment".into(),
                "Create a new task in the destination environment.".into(),
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
                "the destination project does not contain the task's current working directory"
                    .into(),
                "Choose a project in the same workspace or create a new task in the destination."
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
                        "the destination resolves to a different checkout or worktree"
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
                    "Choose the same physical workspace root or create a new task.".into(),
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
    ) -> Result<(Chat, luna_proto::ExecutionBinding), crate::EngineError> {
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
        if handle.doc().context_control().compact.blocks_new_turn() {
            return Err(crate::EngineError::Other(
                "resolve context compaction before changing history".into(),
            ));
        }
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
        if capability_for(binding.provider_id) == luna_proto::ProviderForkCapability::None {
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
        binding: &luna_proto::ExecutionBinding,
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

    fn rewind_reconciliation_ready(
        &self,
        task_id: &str,
    ) -> Result<(Chat, luna_proto::ExecutionBinding), crate::EngineError> {
        let chat = self
            .workspace
            .chat(task_id)?
            .ok_or_else(|| crate::EngineError::Other("rewind task no longer exists".into()))?;
        if chat.archived || self.sessions.active_turn_id(task_id).is_some() {
            return Err(crate::EngineError::Other(
                "wait for an idle, unarchived task before rewind reconciliation".into(),
            ));
        }
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding)
            .ok_or_else(|| crate::EngineError::Other("rewind task has no binding".into()))?;
        if binding.provider_id != HarnessId::Codex
            || chat.config.as_ref().map(|config| config.harness) != Some(HarnessId::Codex)
        {
            return Err(crate::EngineError::Other(
                "rewind task provider changed".into(),
            ));
        }
        Ok((chat, binding))
    }

    fn conversation_rewind_client(
        &self,
        task_id: &str,
        chat: &Chat,
    ) -> Result<luna_harness::codex::native::CodexNativeClient, crate::EngineError> {
        let config = chat.config.as_ref().ok_or_else(|| {
            crate::EngineError::Other("task has no provider configuration".into())
        })?;
        let execution = self
            .agent_accounts
            .resolve_execution(HarnessId::Codex, config.provider_account_id.as_deref())?;
        let home = execution.codex_home.ok_or_else(|| {
            crate::EngineError::Other("Codex execution home is unavailable".into())
        })?;
        self.task_identity
            .rewind_client(task_id, &home, execution.codex_file_auth)
    }

    fn commit_conversation_rewind(
        &self,
        task_id: &str,
        command_id: &str,
        record: &crate::history_operations::ConversationRewindRecord,
        reverted: bool,
    ) -> Result<CommandReceipt, TaskProjectionError> {
        let (_, binding) = self
            .rewind_reconciliation_ready(task_id)
            .map_err(TaskProjectionError::ProviderUnknown)?;
        if binding.binding_id != record.binding_id
            || binding.generation != record.binding_generation
        {
            return Err(TaskProjectionError::ProviderUnknown(
                crate::EngineError::Other(
                    "rewind binding changed; retained history was not projected".into(),
                ),
            ));
        }
        let source_bytes = self
            .store
            .load_snapshot(&format!(
                "{}:transcript",
                crate::history_operations::rewind_record_id(task_id, &record.command_id)
            ))
            .map_err(|error| TaskProjectionError::ProviderUnknown(error.into()))?
            .ok_or_else(|| {
                TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                    "rewind source snapshot is unavailable".into(),
                ))
            })?;
        let original = SessionDoc::from_snapshot(&source_bytes)
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::ProviderUnknown)?
            .read_entries()
            .map_err(crate::EngineError::from)
            .map_err(TaskProjectionError::ProviderUnknown)?;
        let marker = self.history_marker(format!("history-rewind:{}", record.command_id), format!(
            "Rewound this conversation before message {} in the same provider session. Files and external effects were kept. Audit snapshot: {}.",
            record.message_id, crate::history_operations::rewind_record_id(task_id, &record.command_id),
        ));
        let (_, receipt) = self.doc_host.commit_context_boundary(
            task_id, command_id, Some(&binding.binding_id), Some(binding.generation),
            None, None, &[], &[],
            |doc| {
                if doc.context_control().pending_rewind.as_deref() != Some(record.command_id.as_str()) {
                    return Err(crate::EngineError::Other("rewind intent changed before projection".into()));
                }
                if reverted {
                    if doc.read_entries()? != original {
                        return Err(crate::EngineError::Other("LUNA transcript changed while rewind was pending; keep later rows visible".into()));
                    }
                    doc.replace_conversation_entries(&record.entries, &record.checkpoints)?;
                    doc.push_message(&marker)?;
                    doc.clear_context_usage()?;
                }
                let mut context = doc.context_control();
                context.pending_rewind = None;
                context.provider_summary = None;
                context.notice = Some(if reverted {
                    "Conversation rewound in the same Codex session. Files kept; Git checkpoint restore is a separate operation.".into()
                } else {
                    "Rewind readback verified the original conversation. No history was removed; files kept.".into()
                });
                doc.set_context_control(&context)?;
                Ok(())
            },
            |_| Ok(()),
        ).map_err(|error| TaskProjectionError::ProviderUnknown(crate::EngineError::Other(error.message().to_string())))?;
        // The same binding and in-memory provider route remain current. Do not
        // install the clear-context no-resume tombstone: continuation must use
        // the existing Codex thread ID.
        Ok(receipt)
    }

    fn hidden_turn_prefix(
        &self,
        chat: &Chat,
        binding: &luna_proto::ExecutionBinding,
    ) -> Result<Vec<String>, crate::EngineError> {
        let key = format!(
            "luna-hidden-turn-prefix-v1:{}:{}",
            chat.id, binding.provider_task_id
        );
        let Some(bytes) = self.store.load_snapshot(&key)? else {
            // Older sides retain strict full-history equality. Never infer a
            // prefix from a matching suffix of today's provider history.
            return Ok(Vec::new());
        };
        let prefix: HiddenTurnPrefix = serde_json::from_slice(&bytes)?;
        if prefix.provider != binding.provider_id
            || prefix.cwd != chat.cwd.clone().unwrap_or_else(|| ".".into())
            || prefix.provider_account_id
                != chat
                    .config
                    .as_ref()
                    .and_then(|config| config.provider_account_id.clone())
        {
            return Err(crate::EngineError::Other(
                "Inherited provider history belongs to a different account or checkout".into(),
            ));
        }
        Ok(prefix.turns)
    }

    fn fork_hidden_turns(
        &self,
        source: &Chat,
        source_binding: &luna_proto::ExecutionBinding,
        plan: &crate::history_operations::HistoryPlan,
        side_chat: bool,
    ) -> Result<Vec<String>, crate::EngineError> {
        let mut source_turns = self.hidden_turn_prefix(source, source_binding)?;
        if side_chat {
            if plan
                .entries
                .iter()
                .rev()
                .find(|entry| entry.role == MessageRole::Assistant)
                .is_some_and(|entry| !plan.checkpoints.contains_key(&entry.id))
            {
                return Err(crate::EngineError::Other(
                    "This history ends without an exact provider checkpoint. Create a context snapshot instead of a native fork.".into(),
                ));
            }
            for entry in &plan.entries {
                if entry.role == MessageRole::Assistant
                    && let Some(checkpoint) = plan.checkpoints.get(&entry.id)
                {
                    source_turns.push(checkpoint.clone());
                }
            }
        }
        Ok(source_turns)
    }

    /// Display segments have no independent provider boundary. Compare the
    /// entire retained prefix before creating a native side fork, so skipping
    /// their absent checkpoints cannot also hide a missing whole native turn.
    async fn validate_side_fork_native_prefix(
        &self,
        source: &Chat,
        binding: &luna_proto::ExecutionBinding,
        plan: &crate::history_operations::HistoryPlan,
        expected: &[String],
    ) -> Result<(), crate::EngineError> {
        use luna_proto::ProviderForkBoundary;
        let fail = || {
            crate::EngineError::Other(
                "Provider and LUNA retained history differ; native side fork was not sent".into(),
            )
        };
        let native = if binding.provider_id == HarnessId::Codex {
            let anchor = match &plan.provider_boundary {
                ProviderForkBoundary::BeforeCheckpoint { checkpoint_id }
                | ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
                    Some(checkpoint_id.as_str())
                }
                ProviderForkBoundary::Tip => expected.last().map(String::as_str),
            };
            // Empty native history has no anchor to read through the Codex
            // API. Its exhaustive fork checkpoint map must remain empty at
            // publication; a nonempty map is refused below.
            let Some(anchor) = anchor else {
                return Ok(());
            };
            let client = self.conversation_rewind_client(&source.id, source)?;
            let cwd = source.cwd.clone().unwrap_or_else(|| ".".into());
            // prepare_rewind reads thread history and computes a prefix only;
            // it never issues thread/revert or any other history mutation.
            let read = tokio::time::timeout(
                Duration::from_secs(30),
                client.prepare_rewind(&binding.provider_task_id, anchor, &cwd),
            )
            .await
            .map_err(|_| {
                crate::EngineError::Other("native side fork history read timed out".into())
            })??;
            read.original
                .iter()
                .map(|turn| turn.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(fail)?
        } else {
            let read = self
                .native_turns(binding.provider_id, self.native_scope(source, binding))
                .await?;
            // OpenCode keeps completed compaction and preceding history in
            // its native store, so its fork contract still supports this
            // exact prefix. Claude's native fork refuses compacted history.
            if read.compacted && binding.provider_id == HarnessId::ClaudeCode {
                return Err(fail());
            }
            read.turn_starts
        };
        let end = match &plan.provider_boundary {
            ProviderForkBoundary::Tip => native.len(),
            ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => native
                .iter()
                .position(|turn| turn == checkpoint_id)
                .ok_or_else(fail)?,
            ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
                native
                    .iter()
                    .position(|turn| turn == checkpoint_id)
                    .ok_or_else(fail)?
                    + 1
            }
        };
        if expected != &native[..end] {
            return Err(fail());
        }
        Ok(())
    }

    fn preserve_fork_hidden_turns(
        &self,
        target_task: &str,
        source: &Chat,
        source_binding: &luna_proto::ExecutionBinding,
        result: &luna_proto::ProviderForkResult,
        plan: &crate::history_operations::HistoryPlan,
        side_chat: bool,
    ) -> Result<(), crate::EngineError> {
        let source_turns = self.fork_hidden_turns(source, source_binding, plan, side_chat)?;
        // These adapters return one mapping per native turn. OpenCode maps
        // every message instead, so its full fork prefix is read back before
        // reaching this publication path.
        if side_chat
            && matches!(
                source_binding.provider_id,
                HarnessId::ClaudeCode | HarnessId::Codex
            )
            && result.checkpoint_map.len() != source_turns.len()
        {
            return Err(crate::EngineError::Other(
                "Provider fork contains a different inherited turn prefix; fork was not published"
                    .into(),
            ));
        }
        let turns = source_turns
            .iter()
            .map(|turn| {
                result.checkpoint_map.get(turn).cloned().ok_or_else(|| {
                    crate::EngineError::Other(
                        "Provider fork did not map an inherited history checkpoint".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let prefix = HiddenTurnPrefix {
            provider: source_binding.provider_id,
            provider_account_id: source
                .config
                .as_ref()
                .and_then(|config| config.provider_account_id.clone()),
            cwd: source.cwd.clone().unwrap_or_else(|| ".".into()),
            turns,
        };
        let key = format!(
            "luna-hidden-turn-prefix-v1:{target_task}:{}",
            result.provider_session_id
        );
        if let Some(prior) = self.store.load_snapshot(&key)? {
            if serde_json::from_slice::<HiddenTurnPrefix>(&prior)? != prefix {
                return Err(crate::EngineError::Other(
                    "Conflicting inherited provider history proof; fork was not published".into(),
                ));
            }
            return Ok(());
        }
        self.store
            .save_snapshot(&key, &serde_json::to_vec(&prefix)?)?;
        Ok(())
    }

    async fn fork_provider_history(
        &self,
        command_id: &str,
        chat: &Chat,
        binding: &luna_proto::ExecutionBinding,
        boundary: luna_proto::ProviderForkBoundary,
        target_name: Option<&str>,
        require_current_binding: bool,
    ) -> Result<luna_proto::ProviderForkResult, TaskProjectionError> {
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
        } else if binding.provider_id == HarnessId::ClaudeCode {
            // CC-10: Claude's native transcripts live under its config dir.
            let (claude_home, _) = self.agent_accounts.skill_config_dirs();
            (Some(claude_home.to_string_lossy().into_owned()), false)
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
            luna_harness::ProviderForkProgress::MutationStarted => {
                tracked_progress.lock().unwrap().0 = true;
                Ok(())
            }
            luna_harness::ProviderForkProgress::Acknowledged {
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
                    Ok(None) => Err(luna_harness::HarnessError::Protocol(
                        "history command receipt vanished before provider acknowledgement".into(),
                    )),
                    Err(error) => Err(luna_harness::HarnessError::Protocol(error.to_string())),
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
                    target_name: target_name.map(str::to_owned),
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
    ) -> Result<luna_proto::RunRequest, crate::EngineError> {
        let mut request = self
            .doc_host
            .request_from_chat_row(source_task_id, prompt)
            .ok_or_else(|| {
                crate::EngineError::Other("task execution settings are unavailable".into())
            })?;
        request.review = None;
        request.goal = None;
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
        request.input_items.clear();
        request.worktree = None;
        Ok(request)
    }

    fn queue_initial_history_prompt(
        &self,
        doc: &SessionDoc,
        command_id: &str,
        message_id: &str,
        request: luna_proto::RunRequest,
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

    async fn apply_goal_operation(
        &self,
        command_id: &str,
        task_id: &str,
        operation: luna_proto::GoalOperation,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        operation.validate().map_err(|error| {
            TaskProjectionError::Mutation(crate::EngineError::Other(error.into()))
        })?;
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(TaskProjectionError::Mutation)?
            .ok_or_else(|| {
                TaskProjectionError::Mutation(crate::EngineError::Other(
                    "Goals require an existing task".into(),
                ))
            })?;
        let config = chat.config.ok_or_else(|| {
            TaskProjectionError::Mutation(crate::EngineError::Other(
                "Goals require a provider configuration".into(),
            ))
        })?;
        if config.harness == HarnessId::ClaudeCode {
            let cwd = chat.cwd.clone().unwrap_or_else(|| ".".into());
            return self
                .apply_claude_goal_operation(command_id, task_id, &cwd, operation)
                .await;
        }
        if config.harness != HarnessId::Codex {
            return self.reject_context_command(
                command_id,
                CommandReasonCode::UnsupportedOperation,
                "Goals are supported by Codex and Claude Code tasks.",
                "Choose a Codex or Claude Code task with an accepted provider binding.",
            );
        }
        let execution = self
            .agent_accounts
            .resolve_execution(HarnessId::Codex, config.provider_account_id.as_deref())
            .map_err(TaskProjectionError::Mutation)?;
        let home = execution.codex_home.ok_or_else(|| {
            TaskProjectionError::Mutation(crate::EngineError::Other(
                "Codex execution home is unavailable".into(),
            ))
        })?;
        let mut cancellation_guard =
            (!matches!(operation, luna_proto::GoalOperation::Get)).then(|| {
                NativeMetadataCommandGuard {
                    doc_host: &self.doc_host,
                    command_id,
                    armed: true,
                }
            });
        let result = self
            .task_identity
            .goal_operation(
                task_id,
                &home,
                execution.codex_file_auth,
                chat.cwd.as_deref().unwrap_or("."),
                &operation,
            )
            .await;
        // Transport loss after dispatch is unknown; a correlated provider
        // rejection proves this control did not apply and can be reported failed.
        let mut readback = match result {
            Ok(readback) => readback,
            Err(error) => {
                if let Some(guard) = &mut cancellation_guard {
                    guard.armed = false;
                }
                return Err(
                    if matches!(operation, luna_proto::GoalOperation::Get)
                        || matches!(
                            &error,
                            crate::EngineError::Harness(
                                luna_harness::HarnessError::RejectedBeforeAcceptance(_)
                            )
                        )
                    {
                        TaskProjectionError::Mutation(error)
                    } else {
                        TaskProjectionError::ProviderUnknown(error)
                    },
                );
            }
        };
        if matches!(operation, luna_proto::GoalOperation::Set { .. })
            && readback
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == luna_proto::GoalStatus::Active)
            && !readback.continuation_ready
        {
            let request = self
                .doc_host
                .request_from_chat_row(task_id, "")
                .ok_or_else(|| {
                    TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                        "Goal runtime configuration is unavailable".into(),
                    ))
                })?;
            if request.provider_account_id != config.provider_account_id
                || request.cwd != chat.cwd.as_deref().unwrap_or(".")
            {
                return Err(TaskProjectionError::ProviderUnknown(
                    crate::EngineError::Other(
                        "Goal account or checkout changed before attachment".into(),
                    ),
                ));
            }
            let expected_thread = readback.thread_id.clone();
            let expected_generation = readback.binding_generation;
            let attachment = self
                .sessions
                .attach_existing_goal(
                    task_id,
                    command_id,
                    request,
                    &expected_thread,
                    expected_generation,
                )
                .await;
            readback = self
                .task_identity
                .goal_operation(
                    task_id,
                    &home,
                    execution.codex_file_auth,
                    chat.cwd.as_deref().unwrap_or("."),
                    &luna_proto::GoalOperation::Get,
                )
                .await
                .map_err(TaskProjectionError::ProviderUnknown)?;
            if readback.thread_id != expected_thread
                || readback.binding_generation != expected_generation
            {
                return Err(TaskProjectionError::ProviderUnknown(
                    crate::EngineError::Other("Goal binding changed after attachment".into()),
                ));
            }
            if readback
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == luna_proto::GoalStatus::Active)
            {
                if let Err(error) = attachment {
                    return Err(TaskProjectionError::ProviderUnknown(error));
                }
                if !readback.continuation_ready {
                    return Err(TaskProjectionError::ProviderUnknown(
                        crate::EngineError::Other(
                            "Goal runtime did not attach; resume was not retried".into(),
                        ),
                    ));
                }
            }
        }
        let receipt = self
            .doc_host
            .complete_goal_operation(task_id, command_id, &readback)
            .map_err(|error| {
                if matches!(operation, luna_proto::GoalOperation::Get) {
                    TaskProjectionError::Commit(error)
                } else {
                    TaskProjectionError::ProviderUnknown(error)
                }
            })?;
        if let Some(guard) = &mut cancellation_guard {
            guard.armed = false;
        }
        Ok(Some(receipt))
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
        // A completed turn can retain an idle native writer. The maintenance
        // client needs exclusive ownership, and the next dispatch must wait
        // until compaction and its durable readback have finished.
        let _compact_transition = if config.harness == HarnessId::Codex {
            Some(
                self.sessions
                    .prepare_native_compaction(task_id)
                    .await
                    .map_err(TaskProjectionError::Mutation)?,
            )
        } else {
            None
        };
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
            // OpenCode reports completion only on `compaction.ended` and
            // bounds its own wait after admission; a cold server boot must
            // not count against that bound (OCF-113).
            HarnessId::Opencode => {
                self.sessions
                    .compact_context(
                        HarnessId::Opencode,
                        binding.provider_task_id,
                        cwd,
                        config.model,
                    )
                    .await
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
        // The provider authoritatively did not apply it (refused before
        // admission, or reported a failure): history is unchanged, so the
        // next turn is not blocked behind an unknown outcome.
        if let Err(crate::EngineError::Harness(
            luna_harness::HarnessError::RejectedBeforeAcceptance(reason),
        )) = &result
        {
            context.compact.status = ContextOperationStatus::Failed;
            context.compact.finished_at_ms = Some(now);
            context.compact.detail = Some(format!("Compaction did not run: {reason}"));
            let receipt = self
                .doc_host
                .complete_context_operation(
                    task_id,
                    command_id,
                    CommandReceiptStatus::Failed,
                    false,
                    Some(CommandReceiptReason {
                        code: CommandReasonCode::OperationFailed,
                        message: context.compact.detail.clone().unwrap_or_default(),
                    }),
                    Some("The conversation is unchanged; continue, or try compacting again."),
                    &context,
                    false,
                )
                .map_err(TaskProjectionError::Commit)?;
            return Ok(Some(receipt));
        }
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
            || requested.message_ids.len() > luna_proto::MAX_CONTEXT_HANDOFF_MESSAGES
        {
            return Err(crate::EngineError::Other(format!(
                "context handoff must contain 1–{} visible messages and at most {} bytes",
                luna_proto::MAX_CONTEXT_HANDOFF_MESSAGES,
                luna_proto::MAX_CONTEXT_HANDOFF_BYTES
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
                luna_doc::MessageRole::User => "User",
                luna_doc::MessageRole::Assistant => "Assistant",
                luna_doc::MessageRole::System => {
                    return Err(crate::EngineError::Other(
                        "system messages cannot be included in a provider handoff".into(),
                    ));
                }
            };
            let visible = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(text.trim()),
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
            let remaining = luna_proto::MAX_CONTEXT_HANDOFF_BYTES.saturating_sub(bytes);
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

    /// A saved but unstarted task still has an initial provider choice. The
    /// command lane is held by admission, so queue/run commands cannot race
    /// these checks and the config projection. Forks and imports always use
    /// an explicit context boundary, even if their own transcript is empty.
    fn has_pristine_provider_choice(
        &self,
        task_id: &str,
        chat: &luna_proto::Chat,
    ) -> Result<bool, crate::EngineError> {
        let Some(identity) = self.task_identity.view(task_id) else {
            return Ok(false);
        };
        if identity.task.origin != luna_proto::TaskOrigin::LunaCreated
            || identity.task.tombstoned
            || identity.task.lineage.source_task_id.is_some()
            || identity.current_binding.is_some()
            || !identity.bindings.is_empty()
            || chat.source_context.is_some()
            || chat
                .harness_session_id
                .as_ref()
                .is_some_and(|id| !id.is_empty())
            || chat
                .child_thread
                .as_ref()
                .is_some_and(|child| !child.initial_prompt.is_empty())
            || self.sessions.has_live_run(task_id)
            || self.sessions.turn_in_flight(task_id)
        {
            return Ok(false);
        }
        if let Some(sides) = &self.side_chats
            && sides
                .layout_identity(task_id)
                .map_err(|error| crate::EngineError::Other(error.to_string()))?
                .is_some()
        {
            return Ok(false);
        }
        let handle = self.doc_host.open(task_id)?;
        let doc = handle.doc();
        let context = doc.context_control();
        Ok(doc.read_entries()?.is_empty()
            && doc.read_queue()?.is_empty()
            && doc.read_commands()?.is_empty()
            && context.binding_generation.is_none()
            && context.last_boundary.is_none()
            && context.pending_handoff.is_none()
            && context.pending_rewind.is_none())
    }

    fn apply_context_boundary(
        &self,
        command_id: &str,
        task_id: &str,
        target_config: ChatConfig,
        kind: ContextBoundaryKind,
        handoff: Option<ContextHandoff>,
        initial_only: bool,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        self.apply_context_boundary_with_transfer(
            command_id,
            task_id,
            target_config,
            kind,
            handoff,
            initial_only,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_context_boundary_with_transfer(
        &self,
        command_id: &str,
        task_id: &str,
        target_config: ChatConfig,
        kind: ContextBoundaryKind,
        handoff: Option<ContextHandoff>,
        initial_only: bool,
        mut transfer: Option<crate::context_transfers::TransferRecord>,
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
        let current_config = chat.config.clone().ok_or_else(|| {
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
        if kind == ContextBoundaryKind::ProviderSwitched
            && handoff.is_none()
            && transfer.is_none()
            && recovery.is_none()
            && self
                .has_pristine_provider_choice(task_id, &chat)
                .map_err(TaskProjectionError::Mutation)?
        {
            let owned_task_id = task_id.to_string();
            let (_, receipt) =
                self.commit_task_projection(command_id, task_id, move |registry| {
                    if !registry.set_chat_config(&owned_task_id, &target_config)? {
                        return Err(crate::EngineError::Other(
                            "task vanished while choosing its initial provider".into(),
                        ));
                    }
                    Ok(())
                })?;
            return Ok(Some(receipt));
        }
        if initial_only {
            return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                "An initial provider choice requires an unstarted chat with no queued work. Use the explicit provider-switch action.".into(),
            )));
        }
        let cwd = chat.cwd.unwrap_or_else(|| ".".into());
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
        let previous_transfer_receipts = self
            .doc_host
            .context_control(task_id)
            .map_err(TaskProjectionError::Mutation)?
            .context_transfers;
        let mut cancelled_transfers = crate::context_transfers::records(&self.store, task_id)
            .map_err(TaskProjectionError::Mutation)?
            .into_iter()
            .filter(|record| {
                record.receipt.target.task_id == task_id
                    && matches!(
                        record.receipt.status,
                        luna_proto::ContextTransferStatus::Planned
                            | luna_proto::ContextTransferStatus::Unknown
                    )
            })
            .collect::<Vec<_>>();
        for record in &mut cancelled_transfers {
            record.receipt.status = luna_proto::ContextTransferStatus::Failed;
            record.receipt.updated_at_ms = crate::now_ms();
            record.receipt.reason = Some("An explicit context boundary discarded this pending transfer; it will not be reinjected.".into());
        }
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
                let mut state = ContextControlState {
                    binding_generation: Some(binding.generation),
                    compact: CompactOperation::default(),
                    last_boundary: Some(boundary),
                    pending_handoff: handoff_for_state.clone(),
                    last_handoff: handoff_for_state.clone(),
                    context_transfers: previous_transfer_receipts,
                    provider_summary: None,
                    notice: Some(match kind {
                        ContextBoundaryKind::Cleared => {
                            "Old provider context was cleared. The next prompt starts a fresh provider session; LUNA history and files remain."
                                .into()
                        }
                        ContextBoundaryKind::ProviderSwitched if handoff_for_state.is_some() || transfer.is_some() => {
                            "A fresh provider binding will receive only the selected visible-history handoff."
                                .into()
                        }
                        ContextBoundaryKind::ProviderSwitched => {
                            "A fresh provider binding will start without prior model context.".into()
                        }
                    }),
                    ..Default::default()
                };
                let mut local_snapshots = Vec::new();
                for record in cancelled_transfers {
                    crate::context_transfers::project(&mut state, record.receipt.clone());
                    local_snapshots.push((crate::context_transfers::key(&record.receipt.id), serde_json::to_vec(&record).map_err(|error| TaskProjectionError::Mutation(error.into()))?));
                }
                if let Some(record) = transfer.as_mut() {
                    record.receipt.target.provider = target_provider;
                    record.receipt.target.binding_id = Some(binding.binding_id.clone());
                    record.receipt.target.binding_generation = Some(binding.generation);
                    record.receipt.target.context_key = crate::context_transfers::context_key(Some(&binding.binding_id), Some(binding.generation), &state).map_err(TaskProjectionError::Mutation)?;
                    record.target_native_id = None;
                    crate::context_transfers::project(&mut state, record.receipt.clone());
                    local_snapshots.extend(crate::context_transfers::snapshots_for_new(&self.store, record).map_err(TaskProjectionError::Mutation)?);
                }
                let transfer_marker = transfer.as_ref().map(|record| record.receipt.clone());
                let snapshot_refs = local_snapshots.iter().map(|(id, bytes)| (id.as_str(), bytes.as_slice())).collect::<Vec<_>>();
                let boundary_text = state.notice.clone().unwrap_or_default();
                self.doc_host.commit_context_boundary(
                    task_id,
                    command_id,
                    Some(&binding.binding_id),
                    Some(binding.generation),
                    identity_snapshot,
                    transition.as_ref(),
                    &snapshot_refs,
                    &[],
                    move |doc| {
                        doc.clear_context_usage()?;
                        // LUNA history survives the boundary, and so do its
                        // "Sent as goal" / "Goal achieved" marks.
                        let mut state = state.clone();
                        state.goal_marks = doc.context_control().goal_marks;
                        doc.set_context_control(&state)?;
                        if let Some(receipt) = transfer_marker.as_ref() {
                            self.insert_context_transfer_marker(doc, receipt)?;
                        }
                        if !doc
                            .read_entries()?
                            .iter()
                            .any(|entry| entry.id == boundary_id)
                        {
                            doc.push_message(&luna_doc::SessionMessageEntry {
                                id: boundary_id,
                                role: luna_doc::MessageRole::System,
                                parts: vec![luna_doc::MessagePart::Text {
                                    id: "boundary".into(),
                                    text: boundary_text,
                                }],
                                created_at: crate::now_ms(),
                                device_id: self.doc_host.device_id().to_string(),
                                status: Some(luna_doc::MessageStatus::Complete),
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
        send_fence: Option<&crate::cross_task_tools::ApprovedSendFence>,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        match intent {
            TaskCommandIntent::RepairWorkspaceIdentity {
                preview_id,
                evidence_digest,
            } => {
                // Admission already owns the task-command lane. Exempt only
                // this exact pending repair receipt from the quiescence check.
                let _quiescence = self
                    .doc_host
                    .prepare_workspace_identity_repair(task_id, Some(command_id))
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                let receipt = self
                    .task_identity
                    .apply_workspace_identity_repair(
                        task_id,
                        command_id,
                        &preview_id,
                        &evidence_digest,
                    )
                    .await?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::ChildOutcome { outcome } => self
                .commit_task_projection(command_id, task_id, |doc| {
                    let mut chat = doc
                        .chat(task_id)?
                        .ok_or_else(|| crate::EngineError::Other("Child is missing".into()))?;
                    let child = chat
                        .child_thread
                        .as_mut()
                        .ok_or_else(|| crate::EngineError::Other("Task is not a child".into()))?;
                    if !child.outcomes.iter().any(|entry| entry.id == outcome.id) {
                        let id = outcome.id.clone();
                        child.outcomes.push(outcome);
                        child_threads::compact_child_outcomes(child);
                        // Undelivered outcomes accumulate while a parent is
                        // unavailable; drop the summary before exceeding the
                        // synced row budget, and refuse rather than wedge sync.
                        if child_threads::child_row_bytes(&chat)
                            > child_threads::CHILD_ROW_BUDGET_BYTES
                            && let Some(child) = chat.child_thread.as_mut()
                            && let Some(entry) =
                                child.outcomes.iter_mut().find(|entry| entry.id == id)
                        {
                            entry.summary = child_threads::CHILD_SUMMARY_OMITTED.into();
                        }
                        if child_threads::child_row_bytes(&chat)
                            > child_threads::CHILD_ROW_BUDGET_BYTES
                        {
                            return Err(crate::EngineError::Other(
                                "Child outcome history is at its synced size limit; open the parent to deliver pending results".into(),
                            ));
                        }
                    }
                    doc.upsert_chat(&chat)?;
                    Ok(())
                })
                .map(|(_, receipt)| Some(receipt)),
            TaskCommandIntent::ChildOutcomeDelivered { outcome_id } => self
                .commit_task_projection(command_id, task_id, |doc| {
                    let mut chat = doc
                        .chat(task_id)?
                        .ok_or_else(|| crate::EngineError::Other("Child is missing".into()))?;
                    let child = chat
                        .child_thread
                        .as_mut()
                        .ok_or_else(|| crate::EngineError::Other("Task is not a child".into()))?;
                    if let Some(outcome) = child
                        .outcomes
                        .iter_mut()
                        .find(|entry| entry.id == outcome_id)
                    {
                        outcome.delivered = true;
                    }
                    child_threads::compact_child_outcomes(child);
                    doc.upsert_chat(&chat)?;
                    Ok(())
                })
                .map(|(_, receipt)| Some(receipt)),
            TaskCommandIntent::CreateChildChat {
                parent_task_id,
                request,
                execution,
                access_grant,
                draft,
            } => self
                .apply_child_creation(
                    command_id,
                    task_id,
                    &parent_task_id,
                    request,
                    execution.as_ref(),
                    access_grant,
                    draft,
                    send_fence,
                )
                .map(Some),
            TaskCommandIntent::CreateChat {
                side_parent_id,
                space_id,
                device_id,
                config,
                branch,
                cwd,
                lineage,
            } => {
                // A temporary Board discussion is never a chat or a task.
                if task_id.starts_with(board_engine::authoring::AUTHORING_ID_PREFIX)
                    && self
                        .store
                        .is_board_authoring_session(task_id)
                        .unwrap_or(false)
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "A temporary Board discussion is not a chat and cannot become one.".into(),
                    )));
                }
                let space_id = space_id.ok_or_else(|| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "A project is required to create a session".into(),
                    ))
                })?;
                if let Some(parent) = side_parent_id.as_deref() {
                    if !self
                        .side_chats
                        .as_ref()
                        .is_some_and(|service| service.reserved_for(task_id, parent))
                    {
                        return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                            "Side chat creation is not reserved".into(),
                        )));
                    }
                } else if let Some(origin) = lineage
                    .parent_task_id
                    .as_deref()
                    .or(lineage.source_task_id.as_deref())
                    .filter(|origin| *origin != task_id)
                {
                    // A lineage claim makes the new chat a side chat of
                    // `origin`; a side chat cannot be that origin.
                    self.board_guard_legacy_creation(
                        origin,
                        luna_proto::BoardChatCreationKind::NativeFork,
                    )
                    .map_err(|error| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            Self::board_error_text(&error),
                        ))
                    })?;
                }
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
                    Some(&identity_space_id),
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
                                let space = doc.space(&space_id)?.ok_or_else(|| {
                                    crate::EngineError::Other(format!("no such space: {space_id}"))
                                })?;
                                let host_device = space.device_id.clone();
                                let child_cwd = cwd.unwrap_or_else(|| space.path.clone());
                                let inherited_parent = side_parent_id
                                    .as_deref()
                                    .map(|parent_id| {
                                        let parent = doc.chat(parent_id)?.ok_or_else(|| {
                                            crate::EngineError::Other("Side chat parent disappeared before creation".into())
                                        })?;
                                        if parent.cwd.as_deref() != Some(child_cwd.as_str())
                                            || parent.branch != branch
                                            || parent.space_id.as_deref() != Some(space.id.as_str())
                                            || parent.device_id != host_device
                                        {
                                            return Err(crate::EngineError::Other(
                                                "Side chat parent checkout changed before creation; open a new side chat".into(),
                                            ));
                                        }
                                        Ok(parent)
                                    })
                                    .transpose()?;
                                let child = Chat {
                                    child_thread: None,
                                    id: owned_task_id.clone(),
                                    device_id: host_device,
                                    title: side_parent_id.as_ref().map(|_| "Side chat".to_string()),
                                    archived: false,
                                    settlement_override: None,
                                    pinned_order: None,
                                    cwd: Some(child_cwd),
                                    branch: None,
                                    checkout_id: inherited_parent.as_ref().and_then(|parent| parent.checkout_id.clone()),
                                    source_context: inherited_parent.and_then(|parent| parent.source_context),
                                    config,
                                    last_message_preview: None,
                                    last_message_at: None,
                                    created_at: chrono::Utc::now(),
                                    harness_session_id: None,
                                    room_gen: Some(2),
                                    harness_session_cwd: None,
                                    space_id: Some(space.id.clone()),
                                    last_seen_at: None,
                                    manually_unread: false,
                                };
                                if let Some(parent) = side_parent_id {
                                    doc.upsert_side_chat(&child, &parent)?;
                                } else {
                                    doc.upsert_chat(&child)?;
                                }
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
                side_chat,
                new_task_id,
                boundary,
                message_id,
                title,
                initial_prompt,
                config,
                source_link,
                review,
            } => {
                // Message-level Fork and native side forks: side chats never
                // fork their conversation (chat-model §1).
                self.board_guard_legacy_creation(
                    task_id,
                    if side_chat {
                        luna_proto::BoardChatCreationKind::NativeFork
                    } else {
                        luna_proto::BoardChatCreationKind::MessageFork
                    },
                )
                .map_err(|error| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        Self::board_error_text(&error),
                    ))
                })?;
                if side_chat
                    && !self
                        .side_chats
                        .as_ref()
                        .is_some_and(|service| service.reserved_for(&new_task_id, task_id))
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Side chat creation is not reserved".into(),
                    )));
                }
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
                if source.space_id.is_none() {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "A project is required to fork a session".into(),
                    )));
                }
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

                let target_title = title
                    .as_ref()
                    .map(|title| title.trim().to_string())
                    .filter(|title| !title.is_empty())
                    .or_else(|| source.title.as_ref().map(|title| format!("{title} (fork)")));
                let provider_name =
                    format!("[LUNA] {}", target_title.as_deref().unwrap_or("New task"));

                // Fail before mutating a provider when inherited display
                // segments cannot be mapped to exact native history.
                let inherited_turns = self
                    .fork_hidden_turns(&source, &binding, &plan, side_chat)
                    .map_err(TaskProjectionError::Mutation)?;
                if side_chat {
                    self.validate_side_fork_native_prefix(&source, &binding, &plan, &inherited_turns)
                        .await
                        .map_err(TaskProjectionError::Mutation)?;
                }

                let provider_result = self
                    .fork_provider_history(
                        command_id,
                        &source,
                        &binding,
                        plan.provider_boundary.clone(),
                        Some(&provider_name),
                        true,
                    )
                    .await?;
                let provider_session_id = provider_result.provider_session_id.clone();
                let remapped_checkpoints = plan
                    .remapped_checkpoints(&provider_result)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                if side_chat && binding.provider_id == HarnessId::Opencode {
                    let mapped = inherited_turns
                        .iter()
                        .map(|turn| provider_result.checkpoint_map.get(turn).cloned())
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                                "Provider fork did not map an inherited turn".into(),
                            ))
                        })?;
                    let mut scope = self.native_scope(&source, &binding);
                    scope.session_id = provider_session_id.clone();
                    let read = self
                        .native_turns(binding.provider_id, scope)
                        .await
                        .map_err(TaskProjectionError::ProviderUnknown)?;
                    if read.turn_starts != mapped {
                        return Err(TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                            "Provider fork contains a different inherited turn prefix; fork was not published".into(),
                        )));
                    }
                }

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
                self.preserve_fork_hidden_turns(&new_task_id, &source, &binding, &provider_result, &plan, side_chat)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                if !side_chat {
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
                }
                let prompt = initial_prompt
                    .map(|prompt| prompt.trim().to_string())
                    .filter(|prompt| !prompt.is_empty());
                let mut authority = Vec::new();
                if let Some(prompt) = prompt.as_deref() {
                    let local_command_id = format!("fork-run:{command_id}");
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
                        &local_command_id,
                        &uuid::Uuid::new_v4().to_string(),
                        request,
                    )
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                    let command = target_doc
                        .read_commands()
                        .map_err(crate::EngineError::from)
                        .map_err(TaskProjectionError::ProviderUnknown)?
                        .into_iter()
                        .find(|row| row.id == local_command_id)
                        .ok_or_else(|| {
                            TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                                "local fork run command vanished before commit".into(),
                            ))
                        })?;
                    authority.push(LocalExecutableChange {
                        doc_id: new_task_id.clone(),
                        kind: LocalExecutableKind::Command,
                        row_id: local_command_id,
                        expected_hash: None,
                        expected_workspace_repair_revision: None,
                        replacement_hash: Some(
                            crate::doc_host::executable_row_hash(&command)
                                .map_err(TaskProjectionError::ProviderUnknown)?,
                        ),
                        replacement_workspace_repair_revision: Some(0),
                    });
                }
                let target_snapshot = target_doc
                    .export_snapshot()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
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
                                &authority,
                                move |registry| {
                                    let side_parent = source_for_registry.id.clone();
                                    let child = Chat {
                                        child_thread: None,
                                        id: new_task_for_registry,
                                        device_id: source_for_registry.device_id,
                                        title: target_title,
                                        archived: false,
                                        settlement_override: None,
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
                                    };
                                    if side_chat {
                                        registry.upsert_side_chat(&child, &side_parent)?;
                                    } else {
                                        registry.upsert_chat(&child)?;
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
                if prompt.is_some() {
                    let _ = self.doc_host.open(&new_task_id);
                }
                Ok(Some(receipt))
            }
            TaskCommandIntent::RewindConversation {
                message_id,
                keep_files,
                review,
            } => {
                use crate::history_operations::{ConversationRewindRecord, save_rewind_record};
                if !keep_files {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Choose Keep files for conversation rewind; preview and restore a Git checkpoint separately.".into(),
                    )));
                }
                let (chat, binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                if crate::history_operations::rewinds_through_native_hooks(binding.provider_id) {
                    return self
                        .rewind_native_same_chat(
                            command_id, task_id, message_id, review, chat, binding,
                        )
                        .await;
                }
                if binding.provider_id != HarnessId::Codex {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Same-chat native rewind is supported only for Codex, Claude Code and OpenCode."
                            .into(),
                    )));
                }
                self.validate_history_review(task_id, &chat, &binding, &review)
                    .map_err(TaskProjectionError::Mutation)?;
                if review.message_id.as_deref() != Some(message_id.as_str()) {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "rewind review selected a different message".into(),
                    )));
                }
                // The revert runs in a separate native client. A parked warm
                // runtime keeps the pre-rewind thread in memory and a routed
                // follow-up would continue from it, so retire it and keep
                // dispatch excluded until this arm commits or fails.
                let _rewind_transition = self
                    .sessions
                    .prepare_native_rewind(task_id)
                    .await
                    .map_err(TaskProjectionError::Mutation)?;
                let handle = self
                    .doc_host
                    .open(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                let plan = plan_history(
                    handle.doc(),
                    HarnessId::Codex,
                    ConversationForkBoundaryKind::BeforeUserMessage,
                    Some(&message_id),
                )
                .map_err(TaskProjectionError::Mutation)?;
                let luna_proto::ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } =
                    &plan.provider_boundary
                else {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "rewind has no exact before-turn boundary".into(),
                    )));
                };
                let client = self
                    .conversation_rewind_client(task_id, &chat)
                    .map_err(TaskProjectionError::Mutation)?;
                let cwd = chat.cwd.clone().unwrap_or_else(|| ".".into());
                let native = tokio::time::timeout(
                    Duration::from_secs(30),
                    client.prepare_rewind(&binding.provider_task_id, checkpoint_id, &cwd),
                )
                .await
                .map_err(|_| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "rewind preflight timed out before mutation".into(),
                    ))
                })?
                .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
                // Every native completed turn must have the same ordered LUNA
                // checkpoint. External/legacy/mixed-binding history is refused.
                let entries = handle
                    .doc()
                    .read_entries()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?;
                let mut document_turns = self.hidden_turn_prefix(&chat, &binding)
                    .map_err(TaskProjectionError::Mutation)?;
                document_turns.extend(
                    crate::history_operations::segment_checkpoints_for_native_comparison(handle.doc(), &entries)
                        .map_err(TaskProjectionError::Mutation)?,
                );
                let native_turns = native
                    .original
                    .iter()
                    .filter_map(|turn| turn.get("id").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                if document_turns
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    != native_turns
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Provider and LUNA turn history differ; rewind was not sent".into(),
                    )));
                }
                // Check the current profile, queue and reviewed task again
                // after asynchronous preflight, before durable staging.
                let (current_chat, current_binding) = self
                    .ensure_history_operation_ready(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                self.validate_history_review(task_id, &current_chat, &current_binding, &review)
                    .map_err(TaskProjectionError::Mutation)?;
                if current_binding.binding_id != binding.binding_id {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "rewind binding changed during preflight".into(),
                    )));
                }
                if self.sessions.has_live_run(task_id) {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "a provider runtime started during rewind preflight; rewind was not sent"
                            .into(),
                    )));
                }
                let cutoff = entries
                    .iter()
                    .position(|entry| entry.id == message_id)
                    .ok_or_else(|| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(
                            "rewind message vanished".into(),
                        ))
                    })?;
                let reviewed_plan = plan_history(
                    handle.doc(),
                    HarnessId::Codex,
                    ConversationForkBoundaryKind::BeforeUserMessage,
                    Some(&message_id),
                )
                .map_err(TaskProjectionError::Mutation)?;
                if reviewed_plan.provider_boundary != plan.provider_boundary {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "rewind turn boundary changed during preflight".into(),
                    )));
                }
                let retained_checkpoints = entries[..cutoff]
                    .iter()
                    .filter_map(|entry| {
                        handle
                            .doc()
                            .provider_checkpoint(&entry.id)
                            .map(|id| (entry.id.clone(), id))
                    })
                    .collect::<HashMap<_, _>>();
                let hidden_turns = self.hidden_turn_prefix(&chat, &binding)
                    .map_err(TaskProjectionError::Mutation)?;
                let retained_ids = hidden_turns.iter().map(String::as_str).chain(entries[..cutoff]
                    .iter()
                    .filter(|entry| entry.role == MessageRole::Assistant)
                    .filter_map(|entry| retained_checkpoints.get(&entry.id).map(String::as_str)))
                    .collect::<Vec<_>>();
                if retained_ids
                    != native
                        .retained
                        .iter()
                        .filter_map(|turn| turn.get("id").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "rewind retained checkpoints differ from provider prefix".into(),
                    )));
                }
                let current_client = self
                    .conversation_rewind_client(task_id, &current_chat)
                    .map_err(TaskProjectionError::Mutation)?;
                if client.codex_home() != current_client.codex_home() {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Codex execution account changed during preflight".into(),
                    )));
                }
                let record = ConversationRewindRecord {
                    command_id: command_id.into(),
                    task_id: task_id.into(),
                    binding_id: binding.binding_id.clone(),
                    binding_generation: binding.generation,
                    provider_account_id: chat
                        .config
                        .as_ref()
                        .and_then(|config| config.provider_account_id.clone()),
                    provider_home: client.codex_home().to_string_lossy().into_owned(),
                    cwd: cwd.clone(),
                    message_id,
                    native,
                    entries: entries[..cutoff].to_vec(),
                    checkpoints: retained_checkpoints,
                };
                let snapshot = handle
                    .doc()
                    .export_snapshot()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?;
                if SessionDoc::from_snapshot(&snapshot)
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?
                    .read_entries()
                    .map_err(crate::EngineError::from)
                    .map_err(TaskProjectionError::Mutation)?
                    != entries
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "LUNA transcript changed during rewind preflight".into(),
                    )));
                }
                save_rewind_record(&self.store, &record, &snapshot)
                    .map_err(TaskProjectionError::Mutation)?;
                let mut context = handle.doc().context_control();
                context.pending_rewind = Some(command_id.into());
                context.notice = Some("Conversation rewind is awaiting provider readback. Later history remains visible; new turns are blocked. Use Reconcile rewind after a restart or uncertain result.".into());
                self.doc_host
                    .persist_context_control(task_id, command_id, &context)
                    .map_err(TaskProjectionError::Mutation)?;
                let progress_state = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let tracked = progress_state.clone();
                let progress = move |_| {
                    tracked.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                };
                let outcome = tokio::time::timeout(
                    Duration::from_secs(30),
                    client.revert_thread(&record.native, &cwd, &progress),
                )
                .await;
                match outcome {
                    Ok(Ok(())) => Ok(Some(self.commit_conversation_rewind(task_id, command_id, &record, true)?)),
                    Ok(Err(error)) if !progress_state.load(std::sync::atomic::Ordering::Acquire) => {
                        let mut context = self.doc_host.context_control(task_id).map_err(TaskProjectionError::Mutation)?;
                        context.pending_rewind = None;
                        context.notice = Some(format!("Rewind was not sent: {error}"));
                        self.doc_host.persist_context_control(task_id, command_id, &context).map_err(TaskProjectionError::Mutation)?;
                        Err(TaskProjectionError::Mutation(error.into()))
                    }
                    Ok(Err(error)) => Err(TaskProjectionError::ProviderUnknown(error.into())),
                    Err(_) => Err(TaskProjectionError::ProviderUnknown(crate::EngineError::Other(
                        "Rewind completion timed out; later history remains visible. Reconcile without repeating revert.".into(),
                    ))),
                }
            }
            TaskCommandIntent::ReconcileConversationRewind { rewind_command_id } => {
                if let Some(receipt) = self
                    .reconcile_native_rewind(command_id, task_id, &rewind_command_id)
                    .await?
                {
                    return Ok(Some(receipt));
                }
                use luna_harness::codex::native::CodexRewindObservation;
                let (chat, binding) = self
                    .rewind_reconciliation_ready(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                let context = self
                    .doc_host
                    .context_control(task_id)
                    .map_err(TaskProjectionError::Mutation)?;
                if context.pending_rewind.as_deref() != Some(rewind_command_id.as_str()) {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "No matching unresolved rewind".into(),
                    )));
                }
                let record = crate::history_operations::load_rewind_record(
                    &self.store,
                    task_id,
                    &rewind_command_id,
                )
                .map_err(TaskProjectionError::Mutation)?;
                if binding.binding_id != record.binding_id
                    || binding.generation != record.binding_generation
                    || binding.provider_task_id != record.native.thread_id
                    || chat.cwd.as_deref().unwrap_or(".") != record.cwd
                    || chat
                        .config
                        .as_ref()
                        .and_then(|config| config.provider_account_id.as_ref())
                        != record.provider_account_id.as_ref()
                {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Rewind identity, account or workspace changed; keep later history visible"
                            .into(),
                    )));
                }
                let client = self
                    .conversation_rewind_client(task_id, &chat)
                    .map_err(TaskProjectionError::Mutation)?;
                if client.codex_home() != std::path::Path::new(&record.provider_home) {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other("Codex execution account/home changed; reconcile using the original account".into())));
                }
                let observed = tokio::time::timeout(
                    Duration::from_secs(30),
                    client.observe_rewind(&record.native, &record.cwd),
                )
                .await
                .map_err(|_| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Rewind readback timed out; sends remain blocked".into(),
                    ))
                })?
                .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
                match observed {
                    CodexRewindObservation::Reverted => Ok(Some(self.commit_conversation_rewind(task_id, command_id, &record, true)?)),
                    CodexRewindObservation::Original => Ok(Some(self.commit_conversation_rewind(task_id, command_id, &record, false)?)),
                    CodexRewindObservation::Diverged => Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "Provider history differs from both saved prefixes. Later LUNA history stays visible and sends remain blocked.".into(),
                    ))),
                }
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
                        None,
                        true,
                    )
                    .await?;
                let provider_session_id = provider_result.provider_session_id.clone();
                let remapped_checkpoints = plan
                    .remapped_checkpoints(&provider_result)
                    .map_err(TaskProjectionError::ProviderUnknown)?;
                self.preserve_fork_hidden_turns(task_id, &source, &binding, &provider_result, &plan, false)
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
                            let admission_id = run_command_id.clone();
                            let (_, receipt) = self.doc_host.commit_context_boundary(
                                task_id,
                                command_id,
                                Some(&new_binding.binding_id),
                                Some(new_binding.generation),
                                identity_snapshot,
                                None,
                                &extra,
                                &[admission_id.as_str()],
                                move |doc| {
                                    doc.replace_conversation_entries(
                                        &plan_entries,
                                        &plan_checkpoints,
                                    )?;
                                    doc.push_message(&marker)?;
                                    let mut context = ContextControlState {
                                        goal_marks: doc.context_control().goal_marks,
                                        ..Default::default()
                                    };
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
                        luna_proto::ProviderForkBoundary::ThroughCheckpoint {
                            checkpoint_id: recovery_view.source_tip_checkpoint_id.clone(),
                        },
                        None,
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
                self.preserve_fork_hidden_turns(task_id, &source, &old_binding, &provider_result, &plan, false)
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
                                &[],
                                move |doc| {
                                    doc.replace_conversation_entries(
                                        &plan_entries,
                                        &plan_checkpoints,
                                    )?;
                                    doc.push_message(&marker)?;
                                    let mut context = ContextControlState {
                                        goal_marks: doc.context_control().goal_marks,
                                        ..Default::default()
                                    };
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
                let synced_title = title.clone();
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
                // One rename for a ticket and its working chat (title sync).
                self.board_chat_renamed(task_id, &synced_title).await;
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
                            luna_proto::compare_pin_order(
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
                        let order = luna_proto::pin_order_between(left, right)
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
            TaskCommandIntent::SetSettled { settled } => {
                if settled {
                    let pending = self.sessions.has_live_run(task_id)
                        || self
                            .sessions
                            .session_status(task_id)
                            .is_some_and(|session| {
                                matches!(
                                    session.status,
                                    luna_proto::SessionStatus::Working
                                        | luna_proto::SessionStatus::AwaitingApproval
                                        | luna_proto::SessionStatus::AwaitingInput
                                )
                            });
                    let queued = self
                        .doc_host
                        .open(task_id)
                        .map_err(TaskProjectionError::Mutation)?
                        .doc()
                        .read_queue()
                        .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
                    if pending || !queued.is_empty() {
                        return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                            "Finish the active run or review its queued work before settling this task"
                                .into(),
                        )));
                    }
                }
                let owned_task_id = task_id.to_string();
                let (_, receipt) =
                    self.commit_task_projection(command_id, task_id, move |doc| {
                        let at = doc
                            .chat(&owned_task_id)?
                            .map(|chat| {
                                chrono::Utc::now()
                                    .max(chat.last_message_at.unwrap_or(chat.created_at))
                            })
                            .unwrap_or_else(chrono::Utc::now);
                        if settled {
                            doc.set_chat_seen(&owned_task_id, at)?;
                        }
                        doc.set_chat_settlement_override(
                            &owned_task_id,
                            &luna_proto::ChatSettlementOverride {
                                disposition: if settled {
                                    luna_proto::SettlementDisposition::Settled
                                } else {
                                    luna_proto::SettlementDisposition::Active
                                },
                                at,
                            },
                        )?;
                        Ok(())
                    })?;
                Ok(Some(receipt))
            }
            TaskCommandIntent::SetArchived { archived } => {
                if archived && self.sessions.has_live_background_work(task_id) {
                    return Err(TaskProjectionError::Mutation(crate::EngineError::Other(
                        "The task has live background work; stop it before archiving.".into(),
                    )));
                }
                let native = self.task_identity.has_native_codex_binding(task_id);
                let _archive_transition = if native {
                    Some(
                        self.sessions
                            .prepare_native_archive(task_id)
                            .await
                            .map_err(TaskProjectionError::Mutation)?,
                    )
                } else {
                    None
                };
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
                self.board_guard_container_change(task_id).map_err(|error| {
                    TaskProjectionError::Mutation(crate::EngineError::Other(
                        Self::board_error_text(&error),
                    ))
                })?;
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
                let provider_bound = self
                    .task_identity
                    .view(task_id)
                    .is_some_and(|view| view.current_binding.is_some());
                self.store
                    .prepare_task_cleanup(
                        &self.engine_info.device_id,
                        task_id,
                        &self.cleanup_boot_id,
                    )
                    .map_err(|error| {
                        TaskProjectionError::Mutation(crate::EngineError::Other(error.to_string()))
                    })?;
                let owned_task_id = task_id.to_string();
                let mut current = self
                    .current_task_command_snapshot(task_id)
                    .map_err(|error| {
                        TaskProjectionError::Commit(crate::EngineError::Other(error.to_string()))
                    })?;
                current.binding_id = None;
                current.binding_generation = None;
                current.active_turn_id = None;
                let committed = self.task_identity.tombstone_task_with_command(
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
                );
                let (_, receipt) = match committed {
                    Ok(committed) => committed,
                    Err(error) => {
                        if self.workspace.chat(task_id).ok().flatten().is_some()
                            && !self.store.has_applied_task_delete(task_id).unwrap_or(false)
                        {
                            let _ = self.store.abort_task_cleanup(task_id);
                        }
                        return Err(error);
                    }
                };
                // The receipt covers the atomic tombstone/index change. Local
                // cleanup has its own persisted status and can resume after a
                // crash; it must not rewrite an Applied receipt as Failed.
                self.cleanup_deleted_task(task_id, provider_bound);
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
            TaskCommandIntent::SetCwd { .. } => self.reject_context_command(
                command_id,
                CommandReasonCode::UnsupportedOperation,
                "A task's working directory is fixed when it is created.",
                "Create a new task in the intended workspace instead of moving this task.",
            ),
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
            TaskCommandIntent::Goal { operation } => {
                self.apply_goal_operation(command_id, task_id, operation)
                    .await
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
                    false,
                )
            }
            TaskCommandIntent::SwitchProvider { config, handoff, initial_only } => {
                self.apply_context_boundary(
                    command_id,
                    task_id,
                    config,
                    ContextBoundaryKind::ProviderSwitched,
                    handoff,
                    initial_only.unwrap_or(false),
                )
            }
            TaskCommandIntent::SetModel { config } => {
                self.apply_model_change(command_id, task_id, config)
            }
            TaskCommandIntent::ApplyContextTransfer { request, preview_id } => {
                self.apply_context_transfer(command_id, task_id, request, &preview_id)
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
                input_items,
                hold_for_turn_end,
                follow_up_behavior,
                execution,
            } => {
                use crate::doc_host::QueueMutationOutcome;
                let (outcome, committed) = self
                    .doc_host
                    .enqueue_queued_message_guarded(
                        command_id,
                        task_id,
                        &message_id,
                        &text,
                        attachments,
                        input_items,
                        hold_for_turn_end,
                        follow_up_behavior,
                        execution,
                        send_fence.map(|fence| (&self.sessions, fence)),
                    )
                    .await?;
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
                input_items,
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
                            && input_items
                                .as_ref()
                                .is_none_or(|inputs| &item.input_items == inputs)
                            && item.delivery_gate.is_none()
                            && matches!(item.status, luna_doc::QueueMessageStatus::Held)
                    });
                let (outcome, committed) = if already_applied {
                    // Crash window: the queue projection committed but the
                    // L07 receipt did not. Replaying the same command proves
                    // its exact next revision/content and finalizes it without
                    // applying a second edit.
                    (FinishQueueEditOutcome::Committed, None)
                } else {
                    self.doc_host
                        .finish_queued_message_edit_task_with_inputs(
                            command_id,
                            task_id,
                            &message_id,
                            &lease_id,
                            &text,
                            &expected_text_hash,
                            &attachments,
                            input_items.as_deref(),
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
                self.doc_host
                    .set_task_command_correlation(command_id, &message_id)
                    .map_err(TaskProjectionError::Mutation)?;
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
                self.doc_host
                    .set_task_command_correlation(command_id, &message_id)
                    .map_err(TaskProjectionError::Mutation)?;
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
            TaskCommandIntent::NativeReview { .. } => {
                unreachable!("native review normalized at admission")
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

/// Most RPC rejections are scoped to the requested capability. A broken
/// transport or a revoked/expired device session invalidates the shared link.
fn should_invalidate_link(error: &RpcError) -> bool {
    matches!(error, RpcError::Closed | RpcError::Transport(_))
        || matches!(error, RpcError::Failed(message) if message.starts_with("remote authorization denied:"))
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
                side_parent_id: None,
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
        | MutateParams::SetProjectExecutionDefaults { .. }
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
        methods::CLONE_REPO
        | methods::FETCH_ALL
        | methods::APPLY_UPDATE
        | methods::UPDATE_PROVIDER => Duration::from_secs(15 * 60),
        methods::EXECUTE_GIT_ACTION => Duration::from_secs(150),
        methods::GENERATE_GIT_COMMIT_MESSAGE => Duration::from_secs(45),
        methods::GET_GIT_PR_STATUS
        | methods::GET_GITHUB_PR_PREVIEW
        | methods::GET_GITHUB_PR_DETAILS => Duration::from_secs(70),
        methods::CREATE_GITHUB_DRAFT_PR => Duration::from_secs(110),
        methods::CREATE_WORKTREE => Duration::from_secs(120),
        _ => Duration::from_secs(30),
    }
}

/// ControlRpc methods that honor `targetDeviceId` (feature-inventory §2.1). Extend this
/// list (plus [`is_stream_method`] for streams) to make more of the surface
/// device-addressable — the handlers themselves need no changes.
fn restore_owner_paths_overlap(root: &std::path::Path, other: &std::path::Path) -> bool {
    other.starts_with(root) || root.starts_with(other)
}

fn forwardable(method: &str) -> bool {
    matches!(
        method,
        methods::GET_TASK_WORK
            | methods::WATCH_TASK_WORK
            | methods::PREVIEW_STOP_TASK_WORK
            | methods::STOP_TASK_WORK
            | methods::LIST_HARNESSES
            | methods::GET_PROVIDER_UPDATES
            | methods::UPDATE_PROVIDER
            | methods::GET_USAGE_SNAPSHOT
            | methods::GET_TITLE_SETTINGS
            | methods::SET_TITLE_SETTINGS
            | methods::SET_HARNESS_ENABLED
            | methods::LIST_MODELS
            | methods::LIST_COMMANDS
            | methods::LIST_COMPOSER_CATALOG
            | methods::QUEUE_COMMAND
            | methods::CREATE_CHILD_THREAD
            | methods::EXECUTE_TASK_COMMAND
            | methods::GET_TASK_COMMAND_RECEIPT
            | methods::GET_TASK_CLEANUP
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
            | methods::GET_GITHUB_PR_PREVIEW
            | methods::GET_GITHUB_PR_DETAILS
            | methods::CREATE_GITHUB_DRAFT_PR
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
            | methods::FETCH_TOOL_BLOB
            | methods::MCP_ELICITATION_URL
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
        methods::WATCH_TASK_WORK
            | methods::WATCH_TASK_COMMANDS
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

/// The transcript watch as delta frames (`luna_doc::transcript_delta`): a
/// full `reset` first, then only changed entries per commit — the whole-Vec
/// serialization here was the per-tick cost that scaled with transcript size.
fn doc_messages_stream(
    rx: watch::Receiver<crate::doc_host::TranscriptSnapshot>,
    doc: std::sync::Arc<luna_doc::SessionDoc>,
) -> BoxStream<'static, serde_json::Value> {
    doc_messages_stream_with_resume(rx, doc, false, None)
}

fn doc_messages_stream_with_resume(
    rx: watch::Receiver<crate::doc_host::TranscriptSnapshot>,
    doc: std::sync::Arc<luna_doc::SessionDoc>,
    resume_enabled: bool,
    known_token: Option<String>,
) -> BoxStream<'static, serde_json::Value> {
    use luna_doc::transcript_delta::{TranscriptFrame, diff_transcript};
    futures::stream::unfold(
        (
            rx,
            None::<crate::doc_host::TranscriptSnapshot>,
            doc,
            None,
            luna_proto::ContextControlState::default(),
            luna_doc::TranscriptBaseline::default(),
            None::<luna_proto::ProviderNotice>,
            known_token,
            None::<Vec<String>>,
        ),
        move |(
            mut rx,
            mut prev,
            doc,
            mut previous_usage,
            mut previous_context,
            mut opening_baseline,
            mut previous_notice,
            mut known_token,
            mut previous_checkpointed,
        )| async move {
            loop {
                if prev.is_some() {
                    rx.changed().await.ok()?;
                }
                // Watchers retain the immutable published snapshot. Each
                // connection used to deep-copy the entire transcript here.
                let current = rx.borrow_and_update().clone();
                let frame = match prev.as_ref() {
                    None if resume_enabled
                        && known_token.as_deref().is_some_and(|token| {
                            !token.is_empty() && token == current.content_token.as_ref()
                        }) =>
                    {
                        TranscriptFrame::Delta {
                            upsert: Vec::new(),
                            append: Vec::new(),
                            remove: Vec::new(),
                            count: current.entries.len(),
                        }
                    }
                    None => TranscriptFrame::reset(&current.entries),
                    Some(prev) if std::sync::Arc::ptr_eq(&prev.entries, &current.entries) => {
                        TranscriptFrame::Delta {
                            upsert: Vec::new(),
                            append: Vec::new(),
                            remove: Vec::new(),
                            count: current.entries.len(),
                        }
                    }
                    Some(prev) => diff_transcript(&prev.entries, &current.entries),
                };
                let replay_baseline = match prev.as_ref() {
                    None => {
                        opening_baseline = luna_doc::TranscriptBaseline::capture(&current.entries);
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
                let token_changed = resume_enabled
                    && prev
                        .as_ref()
                        .is_none_or(|previous| previous.content_token != current.content_token);
                let transcript_token = resume_enabled
                    .then(|| current.content_token.to_string())
                    .filter(|token| !token.is_empty());
                known_token = None;
                prev = Some(current);
                // No-op commits (a second watcher attaching, command-only
                // changes) produce empty deltas — skip the frame entirely.
                let usage = doc.context_usage();
                let context_control = doc.context_control();
                let provider_notice = doc.provider_notice();
                // The first frame and every change carry the set; a turn's
                // checkpoint lands in its own commit just after the entry.
                let checkpointed = doc.checkpointed_entries();
                let checkpoints_changed = previous_checkpointed.as_ref() != Some(&checkpointed);
                if frame.is_empty_delta()
                    && usage == previous_usage
                    && context_control == previous_context
                    && provider_notice == previous_notice
                    && replay_baseline.is_none()
                    && !token_changed
                    && !checkpoints_changed
                {
                    continue;
                }
                previous_usage = usage;
                previous_context = context_control.clone();
                previous_notice = provider_notice.clone();
                let checkpointed = checkpoints_changed.then(|| {
                    previous_checkpointed = Some(checkpointed.clone());
                    checkpointed
                });
                let value = serde_json::to_value(luna_doc::TranscriptUpdate {
                    frame,
                    context_usage: usage,
                    context_control,
                    replay_baseline,
                    provider_notice,
                    transcript_token,
                    checkpointed,
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
                        previous_notice,
                        known_token,
                        previous_checkpointed,
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
) -> Result<(Vec<TaskCommandEvent>, bool), luna_sync::StoreError> {
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
    store
        .bind_appshot_target_environment(&chat.id, &chat.device_id)
        .map_err(|error| error.to_string())?;
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
    let mut events = crate::activity_history::build_activity_envelopes(
        local_device_id,
        chat,
        task_identity.as_ref(),
        &entries,
        &receipts,
        receipts_truncated,
        &run_metadata,
        policy.as_ref(),
    );
    let appshot_records = store
        .appshot_activity_for_task(&chat.id)
        .map_err(|error| error.to_string())?;
    events.extend(crate::activity_history::build_appshot_envelopes(
        local_device_id,
        chat,
        task_identity.as_ref(),
        &appshot_records,
        Some(&entries),
        policy.as_ref(),
    ));
    let count = events.len();
    store
        .replace_activity_events(&chat.id, &events)
        .map_err(|error| error.to_string())?;
    Ok(count)
}

fn cross_task_summary(
    chat: &Chat,
    session: Option<&luna_proto::Session>,
    lineage: luna_proto::TaskLineage,
) -> luna_proto::CrossTaskSummary {
    let live = session.filter(|session| {
        session.device_id == chat.device_id
            && session.updated_at.timestamp_millis().saturating_add(45_000) >= crate::now_ms()
    });
    let indicator = luna_proto::chat_indicator(chat, live);
    let status = serde_json::to_value(indicator)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    luna_proto::CrossTaskSummary {
        agent: chat
            .config
            .as_ref()
            .map(|config| luna_proto::CrossTaskAgent {
                provider: config.harness,
                model: config.model.clone(),
                reasoning: config.reasoning,
                lineage,
            }),
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

fn require_local_control_evidence_scope(
    caller: &luna_proto::CrossTaskSummary,
    target: &luna_proto::CrossTaskSummary,
    task_id: &str,
    authority: &CrossTaskAuthority,
    local_device_id: &str,
) -> Result<(), CrossTaskError> {
    if !authority.delivery.attaches_luna_tools() {
        return Err(CrossTaskError::Unavailable);
    }
    if !authority.granted_scopes.contains("threads.read") {
        return Err(CrossTaskError::Scope);
    }
    if caller.task_id != authority.caller_task_id || target.task_id != task_id {
        return Err(CrossTaskError::InvalidBackendResult);
    }
    if task_id == authority.caller_task_id
        || authority.local_environment_id != local_device_id
        || caller.environment_id != local_device_id
        || target.environment_id != local_device_id
        || caller.project_id.is_none()
        || target.project_id != caller.project_id
        || !caller
            .project_id
            .as_ref()
            .is_some_and(|project| authority.visible_project_ids.contains(project))
    {
        return Err(CrossTaskError::TargetScope);
    }
    Ok(())
}

fn validate_local_control_receipt(
    receipt: &CommandReceipt,
    task_id: &str,
    command_id: &str,
    authority: &CrossTaskAuthority,
    local_device_id: &str,
) -> Result<(), CrossTaskError> {
    if receipt.command_id != command_id
        || receipt.task_id != task_id
        || receipt.snapshot.task_id != task_id
        || receipt.execution_environment.as_deref() != Some(local_device_id)
        || receipt.client_id != "luna-cross-task"
        || receipt.actor_id != format!("luna-tool:{}", authority.caller_task_id)
        || receipt.client_session_id != authority.caller_task_id
        || !matches!(
            receipt.intent.as_str(),
            "createChat" | "stop" | "setArchived" | "moveProject"
        )
    {
        return Err(CrossTaskError::InvalidBackendResult);
    }
    Ok(())
}

async fn local_control_evidence_target(
    rpc: &EngineRpc,
    task_id: &str,
    authority: &CrossTaskAuthority,
) -> Result<(), CrossTaskError> {
    if task_id.is_empty() || task_id.len() > 128 {
        return Err(CrossTaskError::Input);
    }
    if !authority.delivery.attaches_luna_tools() {
        return Err(CrossTaskError::Unavailable);
    }
    if !authority.granted_scopes.contains("threads.read") {
        return Err(CrossTaskError::Scope);
    }
    if task_id == authority.caller_task_id
        || authority.local_environment_id != rpc.engine_info.device_id
    {
        return Err(CrossTaskError::TargetScope);
    }
    let caller = CrossTaskBackend::task(rpc, &authority.caller_task_id)
        .await?
        .ok_or(CrossTaskError::TargetScope)?;
    let target = CrossTaskBackend::task(rpc, task_id)
        .await?
        .ok_or(CrossTaskError::TargetScope)?;
    require_local_control_evidence_scope(
        &caller,
        &target,
        task_id,
        authority,
        &rpc.engine_info.device_id,
    )
}

fn cross_task_queue_command(
    request: &luna_proto::CrossTaskSendRequest,
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
            input_items: Vec::new(),
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
    request: &luna_proto::CrossTaskControlRequest,
    authority: &CrossTaskAuthority,
) -> TaskCommandParams {
    use luna_proto::CrossTaskControlAction as Action;
    let intent = match request.action {
        Action::Create => TaskCommandIntent::CreateChat {
            side_parent_id: None,
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
    async fn workspace(&self, authority: &CrossTaskAuthority) -> Result<Value, CrossTaskError> {
        self.read_agent_workspace(authority)
    }
    // ---- board tools (A4: board/tools.rs, board/context.rs) ----
    fn board_tools(
        &self,
        authority: &CrossTaskAuthority,
    ) -> Vec<luna_proto::RuntimeToolDefinition> {
        self.board_tools_for_authority(authority)
    }
    fn board_context(&self, task_id: &str) -> Option<luna_proto::BoardRuntimeContext> {
        self.board_run_context(task_id)
    }
    async fn call_board_tool(
        &self,
        authority: &CrossTaskAuthority,
        name: &str,
        arguments: Value,
    ) -> Result<Value, CrossTaskError> {
        self.board_tool_invoke(authority, name, arguments).await
    }
    async fn prepare_workspace(
        &self,
        request: crate::cross_task_tools::WorkspaceToolRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<crate::cross_task_tools::PreparedWorkspace, CrossTaskError> {
        self.prepare_agent_workspace(request, authority)
    }
    async fn apply_workspace(
        &self,
        prepared: crate::cross_task_tools::PreparedWorkspace,
        authority: &CrossTaskAuthority,
        fence: &crate::cross_task_tools::ApprovedSendFence,
    ) -> Result<Value, CrossTaskError> {
        self.apply_agent_workspace(prepared, authority, fence).await
    }
    fn child_providers(&self) -> Vec<luna_proto::HarnessId> {
        self.available_child_providers()
    }
    fn capabilities(&self) -> CrossTaskCapabilities {
        CrossTaskCapabilities {
            workspace: self.chat_workbenches.is_some(),
            list: true,
            read: true,
            durable_queue: true,
            approved_queue: true,
            event_wait: true,
            child_creation: true,
            child_models: true,
            automations: true,
        }
    }

    async fn prepare_automation(
        &self,
        request: luna_proto::AutomationToolRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<crate::cross_task_tools::PreparedAutomation, CrossTaskError> {
        let _lane = self
            .doc_host
            .task_command_gate(&authority.caller_task_id)
            .await;
        self.prepare_agent_automation(request, authority)
    }
    async fn apply_approved_automation(
        &self,
        prepared: crate::cross_task_tools::PreparedAutomation,
        authority: &CrossTaskAuthority,
        fence: &crate::cross_task_tools::ApprovedSendFence,
    ) -> Result<Value, CrossTaskError> {
        let _lane = self
            .doc_host
            .task_command_gate(&authority.caller_task_id)
            .await;
        self.apply_agent_automation(prepared, authority, fence)
    }
    async fn list_automations(
        &self,
        request: crate::cross_task_tools::AutomationToolListRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<Value, CrossTaskError> {
        self.list_agent_automations(request, authority)
    }

    async fn child_creation_snapshot(
        &self,
        request: &luna_proto::CreateChildThreadRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<crate::cross_task_tools::PreparedChildCreation, CrossTaskError> {
        self.validate_child_authority(request, authority)?;
        let child_id = crate::cross_task_tools::child_task_id(
            &authority.caller_task_id,
            &request.idempotency_key,
        );
        let execution = if self
            .store
            .task_command_receipt(&format!("child-create:{child_id}"))
            .map_err(|error| CrossTaskError::Backend(error.to_string()))?
            .is_some()
        {
            self.workspace
                .chat(&child_id)
                .map_err(|error| CrossTaskError::Backend(error.to_string()))?
                .and_then(|chat| chat.config)
                .ok_or(CrossTaskError::InvalidBackendResult)?
        } else {
            self.prepare_child_execution(&authority.caller_task_id, request)
                .await
                .map_err(|error| CrossTaskError::Backend(error.to_string()))?
        };
        let snapshot = self
            .current_task_command_snapshot(&child_id)
            .map_err(|_| CrossTaskError::Backend("child snapshot unavailable".into()))?;
        let auth_instance = if matches!(execution.harness, HarnessId::Codex | HarnessId::ClaudeCode)
        {
            Some(
                self.agent_accounts
                    .resolve_execution(execution.harness, execution.provider_account_id.as_deref())
                    .map_err(|error| CrossTaskError::Backend(error.to_string()))?
                    .auth_instance_id,
            )
        } else {
            None
        };
        Ok(crate::cross_task_tools::PreparedChildCreation {
            snapshot,
            execution,
            auth_instance,
        })
    }
    async fn child_model_catalog(
        &self,
        request: luna_proto::ChildModelCatalogRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<luna_proto::ChildModelCatalog, CrossTaskError> {
        self.child_models(request, authority).await
    }
    async fn create_child(
        &self,
        request: luna_proto::CreateChildThreadRequest,
        authority: &CrossTaskAuthority,
        fence: &crate::cross_task_tools::ApprovedChildFence,
    ) -> Result<luna_proto::ChildThreadReceipt, CrossTaskError> {
        self.create_child_approved(request, authority, fence).await
    }
    async fn task(
        &self,
        task_id: &str,
    ) -> Result<Option<luna_proto::CrossTaskSummary>, CrossTaskError> {
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
            identity.task.lineage,
        )))
    }

    async fn control_target_snapshot(
        &self,
        task_id: &str,
        authority: &CrossTaskAuthority,
    ) -> Result<TaskCommandSnapshot, CrossTaskError> {
        local_control_evidence_target(self, task_id, authority).await?;
        let _lane = self.doc_host.task_command_gate(task_id).await;
        local_control_evidence_target(self, task_id, authority).await?;
        let snapshot = self
            .current_task_command_snapshot(task_id)
            .map_err(|_| CrossTaskError::Backend("target snapshot lookup failed".into()))?;
        if snapshot.task_id != task_id {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        Ok(snapshot)
    }

    async fn control_command_receipt(
        &self,
        task_id: &str,
        command_id: &str,
        authority: &CrossTaskAuthority,
    ) -> Result<Option<CommandReceipt>, CrossTaskError> {
        if command_id.is_empty()
            || command_id.len() > 128
            || !command_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(CrossTaskError::Input);
        }
        local_control_evidence_target(self, task_id, authority).await?;
        let _lane = self.doc_host.task_command_gate(task_id).await;
        local_control_evidence_target(self, task_id, authority).await?;
        let receipt = self
            .doc_host
            .task_command_receipt(command_id)
            .map_err(|_| CrossTaskError::Backend("command receipt lookup failed".into()))?;
        if let Some(receipt) = &receipt {
            validate_local_control_receipt(
                receipt,
                task_id,
                command_id,
                authority,
                &self.engine_info.device_id,
            )?;
        }
        Ok(receipt)
    }

    async fn list(
        &self,
        request: luna_proto::CrossTaskListRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<luna_proto::CrossTaskListPage, CrossTaskError> {
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
            })
            .filter(|chat| {
                request
                    .project_id
                    .as_ref()
                    .is_none_or(|id| chat.space_id.as_ref() == Some(id))
            })
            .map(|chat| {
                let session = sessions.iter().find(|session| session.chat_id == chat.id);
                let lineage = self
                    .task_identity
                    .view(&chat.id)
                    .map(|identity| identity.task.lineage)
                    .unwrap_or_default();
                cross_task_summary(&chat, session, lineage)
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
        Ok(luna_proto::CrossTaskListPage {
            tasks,
            next_cursor,
            has_more,
        })
    }

    async fn read(
        &self,
        request: luna_proto::CrossTaskReadRequest,
    ) -> Result<luna_proto::CrossTaskReadPage, CrossTaskError> {
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
            .map(|event| luna_proto::CrossTaskTurnSummary {
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
        Ok(luna_proto::CrossTaskReadPage {
            task,
            turns,
            next_cursor,
            has_more,
            content_untrusted: true,
        })
    }

    async fn enqueue_queue(
        &self,
        request: luna_proto::CrossTaskSendRequest,
        authority: &CrossTaskAuthority,
    ) -> Result<luna_proto::CrossTaskSendReceipt, CrossTaskError> {
        if !authority.explicit_user_request || !authority.granted_scopes.contains("threads.send") {
            return Err(CrossTaskError::UserRequest);
        }
        if request.mode != luna_proto::CrossTaskSendMode::Queue {
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
        Ok(luna_proto::CrossTaskSendReceipt {
            task_id: request.task_id,
            queued_message_id: request.idempotency_key,
            activity_cursor: format!("command:{sequence}"),
            command: receipt,
            delivery_state: "queued".into(),
        })
    }

    async fn enqueue_approved_queue(
        &self,
        request: luna_proto::CrossTaskSendRequest,
        authority: &CrossTaskAuthority,
        fence: &crate::cross_task_tools::ApprovedSendFence,
    ) -> Result<luna_proto::CrossTaskSendReceipt, CrossTaskError> {
        if authority.caller_task_id != fence.source_task_id
            || authority.local_environment_id != self.engine_info.device_id
            || !authority.granted_scopes.contains("threads.read")
            || fence.request != request
            || fence.target_snapshot.task_id != request.task_id
            || fence.expires_at_ms <= crate::now_ms()
        {
            return Err(CrossTaskError::UserRequest);
        }
        local_control_evidence_target(self, &request.task_id, authority).await?;
        let mut command = cross_task_queue_command(&request, authority);
        command.expected_task_revision = Some(fence.target_snapshot.task_revision);
        command.expected_binding_generation = fence.target_snapshot.binding_generation;
        command.expected_turn_id = fence.target_snapshot.active_turn_id.clone();
        let expected_hash = canonical_payload_hash(&command.intent)
            .map_err(|_| CrossTaskError::InvalidBackendResult)?;
        let submitted = self
            .execute_task_command_with_send_fence(command, false, Some(fence))
            .await;
        // The transaction may have committed before an in-memory projection
        // failed. Read the stable command ID instead of replaying the send.
        let readback = self
            .doc_host
            .task_command_receipt(&request.idempotency_key)
            .map_err(|_| {
                CrossTaskError::Backend("queue receipt readback failed; outcome unknown".into())
            })?;
        let receipt = match submitted {
            Ok(receipt) if receipt.status == CommandReceiptStatus::Conflict => receipt,
            Ok(receipt) => readback.unwrap_or(receipt),
            Err(_) => readback.ok_or_else(|| {
                CrossTaskError::Backend("queue outcome unknown; inspect the same command ID".into())
            })?,
        };
        if receipt.payload_hash != expected_hash {
            return Err(CrossTaskError::CommandOutcome {
                status: CommandReceiptStatus::Conflict,
                reason: Some(CommandReasonCode::CommandIdConflict),
            });
        }
        if receipt.command_id != request.idempotency_key
            || receipt.task_id != request.task_id
            || receipt.intent != "queueMessage"
            || receipt.client_id != "luna-cross-task"
            || receipt.actor_id != format!("luna-tool:{}", authority.caller_task_id)
            || receipt.client_session_id != authority.caller_task_id
            || receipt.execution_environment.as_deref() != Some(self.engine_info.device_id.as_str())
        {
            return Err(CrossTaskError::InvalidBackendResult);
        }
        if receipt.status != CommandReceiptStatus::Applied {
            return Err(CrossTaskError::CommandOutcome {
                status: receipt.status,
                reason: receipt.reason.as_ref().map(|reason| reason.code),
            });
        }
        let sequence = receipt
            .event_sequence
            .ok_or(CrossTaskError::InvalidBackendResult)?;
        Ok(luna_proto::CrossTaskSendReceipt {
            task_id: request.task_id,
            queued_message_id: request.idempotency_key,
            activity_cursor: format!("command:{sequence}"),
            command: receipt,
            delivery_state: "queued".into(),
        })
    }

    async fn wait_event(
        &self,
        request: luna_proto::CrossTaskWaitRequest,
    ) -> Result<Option<luna_proto::CrossTaskWaitEvent>, CrossTaskError> {
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
                        luna_proto::SessionStatus::AwaitingApproval
                            | luna_proto::SessionStatus::AwaitingInput
                            | luna_proto::SessionStatus::Errored
                    )
                });
                let completed = task.status == "completed"
                    || row.as_ref().is_some_and(|row| {
                        row.status == luna_proto::SessionStatus::Idle
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
                    return Ok(Some(luna_proto::CrossTaskWaitEvent {
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
        request: luna_proto::CrossTaskControlRequest,
        authority: &CrossTaskAuthority,
        grant: &CrossTaskControlGrant,
    ) -> Result<luna_proto::CrossTaskControlReceipt, CrossTaskError> {
        use luna_proto::CrossTaskControlAction as Action;
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
        Ok(luna_proto::CrossTaskControlReceipt {
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

// Board methods are routed before the main handler body is entered. In debug
// builds that body's poll frame reserves ~1 MiB of stack for all its arms, and
// board operations polled beneath it overflowed a 2 MiB tokio worker (see
// `rpc/board.rs`). Every other method takes the unchanged path below.
impl RpcService for EngineRpc {
    fn handle<'life0, 'life1, 'async_trait>(
        &'life0 self,
        method: &'life1 str,
        params: serde_json::Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RpcReply, RpcError>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        if board::handles(method) {
            return self.handle_board(method, params);
        }
        Box::pin(self.handle_engine(method, params))
    }
}

impl EngineRpc {
    async fn handle_engine(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        if matches!(
            method,
            methods::GET_PROVIDER_DEFAULTS | methods::SET_PROVIDER_DEFAULTS
        ) {
            return self.handle_provider_defaults(method, params).await;
        }
        if side_chats::handles(method) {
            return self.handle_side_chat(method, params).await;
        }
        if context_transfer::handles(method) {
            return self.handle_context_transfer(method, params);
        }
        if method != methods::WATCH_DOC_MESSAGES
            && params
                .get("chatId")
                .or_else(|| params.get("taskId"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| self.derived_fork_document(id))
        {
            return Err(RpcError::Failed(
                "Native-agent transcripts are derived documents, not writable threads".into(),
            ));
        }
        if claude_history::handles(method) {
            return self.handle_native_file_rewind(method, params).await;
        }
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
        if matches!(
            method,
            methods::LIST_GIT_CHECKPOINTS
                | methods::DIFF_GIT_CHECKPOINT
                | methods::PREVIEW_GIT_CHECKPOINT_RESTORE
                | methods::RESTORE_GIT_CHECKPOINT
        ) && params
            .get("targetDeviceId")
            .and_then(|value| value.as_str())
            .is_some_and(|target| target != self.doc_host.device_id())
        {
            return Err(RpcError::Failed(
                "Remote Git checkpoints are unavailable".into(),
            ));
        }
        // Device-addressed routing: forward calls that target another device over its
        // relay. The target compares the id to its own, so forwards cannot loop.
        let project_defaults_mutation = method == methods::MUTATE
            && params.get("op").and_then(|value| value.as_str())
                == Some("setProjectExecutionDefaults");
        if (forwardable(method) || project_defaults_mutation)
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
            methods::LIST_AUTOMATIONS | methods::LIST_AUTOMATION_RUNS | methods::PREVIEW_AUTOMATION | methods::MUTATE_AUTOMATION | methods::GET_AUTOMATION_MUTATION_RECEIPT => self.handle_automations(method, params).await,
            methods::GET_TASK_WORK | methods::WATCH_TASK_WORK | methods::PREVIEW_STOP_TASK_WORK | methods::STOP_TASK_WORK => self.handle_task_work(method, params).await,
            methods::ENGINE_INFO => RpcReply::value(&self.engine_info),
            methods::ENGINE_READY => RpcReply::value(&serde_json::json!({ "ready": true })),
            methods::CREATE_REMOTE_PAIRING => {
                let request: luna_rpc::remote_auth::CreatePairing = parse_params(params)?;
                let store = self
                    .remote_auth
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("remote authority unavailable".into()))?;
                RpcReply::value(&store.create_pairing(request)?)
            }
            methods::LIST_REMOTE_PAIRINGS => {
                let store = self
                    .remote_auth
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("remote authority unavailable".into()))?;
                RpcReply::value(&store.list_pairings())
            }
            methods::REVOKE_REMOTE_PAIRING => {
                #[derive(Deserialize)]
                struct P {
                    id: String,
                }
                let request: P = parse_params(params)?;
                let store = self
                    .remote_auth
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("remote authority unavailable".into()))?;
                RpcReply::value(
                    &serde_json::json!({ "revoked": store.revoke_pairing(&request.id)? }),
                )
            }
            methods::LIST_REMOTE_SESSIONS => {
                let store = self
                    .remote_auth
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("remote authority unavailable".into()))?;
                RpcReply::value(&store.list_sessions())
            }
            methods::REVOKE_REMOTE_SESSION => {
                #[derive(Deserialize)]
                struct P {
                    id: String,
                }
                let request: P = parse_params(params)?;
                let store = self
                    .remote_auth
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("remote authority unavailable".into()))?;
                RpcReply::value(
                    &serde_json::json!({ "revoked": store.revoke_session(&request.id)? }),
                )
            }
            methods::GET_BACKUP_STATUS => {
                let Some(control) = self.backup_control.clone() else {
                    let root = self.managed_backup_root.clone();
                    return backup_rpc_reply(move || {
                        let (destination, last) = match root.as_deref() {
                            Some(root) => match crate::backup::BackupService::managed_offline_status(root) {
                                Ok((destination, last)) => (Some(destination), last),
                                Err(crate::backup::BackupError::UnauthorizedRoot) => (None, None),
                                Err(error) => return Err(BackupControlError::Backup(error)),
                            },
                            None => (None, None),
                        };
                        let automatic_enabled = destination.is_some();
                        Ok(BackupStatus {
                            availability: BackupAvailability::Unavailable,
                            reason: Some("Automatic BB-style backups run after a clean engine shutdown; manual live backup is unavailable.".into()),
                            source_root: root.as_ref().map(|root| root.display().to_string()),
                            destination_dir: destination.map(|path| path.display().to_string()),
                            automatic_enabled,
                            key_configured: false,
                            last_success: last.map(|receipt| BackupSummary {
                                archive_path: receipt.archive.display().to_string(),
                                archive_sha256: receipt.archive_sha256,
                                created_at_ms: receipt.manifest.created_at_ms,
                                total_bytes: receipt.manifest.total_bytes,
                                entry_count: receipt.manifest.entries.len(),
                            }),
                            retention_candidates: 0,
                            staged_restore: None,
                            restore_activation_available: false,
                            restore_reason: "Quit LUNA, then use `luna backup restore <archive>` for a staged, verified offline restore. Live restore is unavailable.".into(),
                        })
                    }).await;
                };
                backup_rpc_reply(move || control.status()).await
            }
            methods::CONFIGURE_BACKUP => {
                let request: BackupConfigureRequest = parse_params(params)?;
                let control = self.backup_control()?;
                backup_rpc_reply(move || control.configure(request)).await
            }
            methods::CONFIGURE_BACKUP_KEY => {
                backup_rpc_reply(|| -> Result<(), BackupControlError> {
                    Err(BackupControlError::KeyNotUsed)
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
                let request: luna_proto::UsageReadRequest = parse_params(params)?;
                let include_limits = request.query.include_limits;
                let mut response = self.usage_cache.read(request).await.map_err(RpcError::Failed)?;
                let snapshot = &mut response.snapshot;
                let still_enabled = Self::usage_providers_for(&self.registry.enabled_set());
                Self::retain_enabled_usage(snapshot, &still_enabled);
                crate::live_usage::attach(snapshot, self.sessions.live_usage(), &still_enabled,
                    include_limits && !crate::usage::synthetic_mode_requested());
                self.sessions.retain_owned_live_usage_rows(&mut snapshot.live_sessions);
                for source in &mut snapshot.summary.sources { source.environment_id = self.engine_info.device_id.clone(); }
                RpcReply::value(&response)
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
                    self.store
                        .bind_appshot_target_environment(&chat.id, &chat.device_id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
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
                    let mut events = crate::activity_history::build_activity_envelopes(
                        &self.engine_info.device_id,
                        chat,
                        identity.as_ref(),
                        &entries,
                        &receipts,
                        receipts_truncated,
                        &run_metadata,
                        policy.as_ref(),
                    );
                    let appshot_records = self
                        .store
                        .appshot_activity_for_task(&chat.id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                    events.extend(crate::activity_history::build_appshot_envelopes(
                        &self.engine_info.device_id,
                        chat,
                        identity.as_ref(),
                        &appshot_records,
                        Some(&entries),
                        policy.as_ref(),
                    ));
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
            methods::PREVIEW_WORKSPACE_IDENTITY_REPAIR => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    task_id: String,
                    #[serde(default)]
                    provider_proof: Option<String>,
                    #[serde(default)]
                    target_device_id: Option<String>,
                }
                let p: P = parse_params(params)?;
                if p.provider_proof.as_deref().unwrap_or("localOnly") != "localOnly" {
                    return Err(RpcError::BadParams("Only localOnly workspace proof is available; provider reads are not enabled".into()));
                }
                if p.target_device_id
                    .as_deref()
                    .is_some_and(|id| id != self.engine_info.device_id)
                    || !self.workspace.is_host(&p.task_id)
                {
                    return Err(RpcError::BadParams(
                        "Workspace repair requires the task's local owning device".into(),
                    ));
                }
                let _lane = self.doc_host.task_command_gate(&p.task_id).await;
                let guard = self
                    .doc_host
                    .prepare_workspace_identity_repair(&p.task_id, None)
                    .await;
                let blocker = guard.as_ref().err().map(ToString::to_string);
                let snapshot = self.current_task_command_snapshot(&p.task_id)?;
                let workspace_revision = self
                    .store
                    .workspace_repair_revision(&p.task_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let preview = self
                    .task_identity
                    .preview_workspace_identity_repair(
                        &p.task_id,
                        snapshot.task_revision,
                        workspace_revision,
                        blocker,
                    )
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&preview)
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
                    .unwrap_or(luna_proto::RuntimeDelivery::Unsupported);
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
                RpcReply::value(&luna_proto::RuntimeContractView {
                    execution: self.sessions.execution_summary(&p.task_id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?,
                    task_id: p.task_id,
                    provider,
                    provider_delivery,
                    effective: receipt,
                    unavailable_reason: match provider_delivery {
                        luna_proto::RuntimeDelivery::Unsupported => Some(
                            "This provider has no verified LUNA runtime instruction or tool delivery path.".into()
                        ),
                        luna_proto::RuntimeDelivery::ClaudeInstructionsOnly => Some(
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
            methods::GET_NATIVE_EXTENSIONS => {
                RpcReply::value(&native_extensions::get(self, parse_params(params)?).await?)
            }
            methods::GET_INTEGRATION_INVENTORY => {
                let p: IntegrationInventoryParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let roots = self.skill_roots(p.project_id.as_deref())?;
                let project_root = roots.project_root.as_deref();
                let view = if p.provider == HarnessId::Codex {
                    let (home, force_file_auth) = self
                        .agent_accounts
                        .codex_usage_target()
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    let mut view = crate::integration_settings::skill_inventory(p.provider, &roots);
                    let workspace = project_root.unwrap_or(&home);
                    // The ordinary Settings read is filesystem-only. A native
                    // app-server process starts only on explicit Refresh.
                    if p.probe_provider && home.is_dir() && workspace.is_dir() {
                        // Catalog reads never need a native login prompt.
                        // Force the file-auth transport even for the native
                        // account; an unavailable catalog stays unavailable.
                        let client = luna_harness::codex::native::CodexNativeClient::new(&home)
                            .for_execution_home(&home, true);
                        let (catalog, mcp_config) = tokio::join!(
                            tokio::time::timeout(
                                std::time::Duration::from_secs(8),
                                client.read_skill_catalog(workspace),
                            ),
                            tokio::time::timeout(
                                std::time::Duration::from_secs(8),
                                client.read_mcp_config(workspace),
                            ),
                        );
                        // A selection change while the provider responds must
                        // not be published under the previous account's home.
                        if self.agent_accounts.codex_usage_target().ok()
                            != Some((home.clone(), force_file_auth))
                        {
                            return Err(RpcError::Failed(
                                "Codex account selection changed during inventory refresh".into(),
                            ));
                        }
                        if let Ok(Ok(catalog)) = catalog {
                            crate::integration_settings::apply_codex_skill_catalog(
                                &mut view,
                                &catalog,
                                &home,
                                project_root,
                            );
                        } else {
                            crate::integration_settings::mark_codex_catalog_unavailable(&mut view);
                        }
                        if let Ok(Ok(mcp_config)) = mcp_config {
                            crate::integration_settings::apply_codex_mcp_config(
                                &mut view,
                                &mcp_config,
                                workspace,
                            );
                        } else {
                            crate::integration_settings::mark_codex_mcp_config_unavailable(
                                &mut view,
                            );
                        }
                    } else if p.probe_provider {
                        crate::integration_settings::mark_codex_catalog_unavailable(&mut view);
                        crate::integration_settings::mark_codex_mcp_config_unavailable(&mut view);
                    }
                    view
                } else if p.provider == HarnessId::ClaudeCode {
                    let mut view = crate::integration_settings::skill_inventory(p.provider, &roots);
                    let harness = self
                        .registry
                        .resolve(HarnessId::ClaudeCode)
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    let session = self
                        .provider_session_for_inventory(
                            HarnessId::ClaudeCode,
                            p.task_id.as_deref(),
                            p.project_id.as_deref(),
                        )
                        .and_then(|id| harness.session_mcp_observation(&id));
                    crate::claude_mcp::augment_inventory(
                        &mut view,
                        harness.as_ref(),
                        project_root,
                        session,
                        p.probe_provider,
                    )
                    .await;
                    view
                } else if p.provider == HarnessId::Opencode {
                    // Read-only MCP rows through the same hooks as Claude's:
                    // the selected chat's session readback, plus OpenCode's
                    // own project readback on Check provider.
                    let mut view = crate::integration_settings::skill_inventory(p.provider, &roots);
                    let harness = self
                        .registry
                        .resolve(HarnessId::Opencode)
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    let session = self
                        .provider_session_for_inventory(
                            HarnessId::Opencode,
                            p.task_id.as_deref(),
                            p.project_id.as_deref(),
                        )
                        .and_then(|id| harness.session_mcp_observation(&id));
                    crate::opencode_mcp::augment_inventory(
                        &mut view,
                        harness.as_ref(),
                        project_root,
                        session,
                        p.probe_provider,
                    )
                    .await;
                    view
                } else {
                    crate::integration_settings::skill_inventory(p.provider, &roots)
                };
                RpcReply::value(&view)
            }
            methods::SET_PROVIDER_MCP_SERVER_ENABLED | methods::READ_PROVIDER_MCP_APP_RESOURCE => {
                let p: crate::claude_mcp::ProviderMcpServerParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let roots = self.skill_roots(p.project_id.as_deref())?;
                let harness = self
                    .registry
                    .resolve(HarnessId::ClaudeCode)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let value = crate::claude_mcp::dispatch(
                    method,
                    p,
                    harness.as_ref(),
                    &self.engine_info.device_id,
                    roots.project_root.as_deref(),
                )
                .await
                .map_err(RpcError::Failed)?;
                RpcReply::value(&value)
            }
            methods::GET_SKILL_DOCUMENT => {
                let p: SkillDocumentParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let roots = self.skill_roots(p.project_id.as_deref())?;
                let document =
                    crate::integration_settings::skill_document(p.provider, &roots, &p.identity)
                        .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&document)
            }
            methods::SAVE_SKILL_DOCUMENT => {
                let p: SaveSkillDocumentParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let roots = self.skill_roots(p.project_id.as_deref())?;
                let document = crate::integration_settings::save_skill_document(
                    p.provider,
                    &roots,
                    p.scope,
                    &p.name,
                    &p.content,
                    p.expected_revision.as_deref(),
                )
                .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&document)
            }
            methods::INSTALL_SKILL => {
                let p: InstallSkillParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let roots = self.skill_roots(p.project_id.as_deref())?;
                let document = crate::integration_settings::install_skill(
                    p.source_provider,
                    &p.source_identity,
                    &p.expected_source_revision,
                    p.destination_provider,
                    p.destination_scope,
                    &roots,
                )
                .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&document)
            }
            methods::REMOVE_SKILL => {
                let p: RemoveSkillParams = parse_params(params)?;
                require_local_integration_target(
                    p.target_device_id.as_deref(),
                    &self.engine_info.device_id,
                )?;
                let roots = self.skill_roots(p.project_id.as_deref())?;
                crate::integration_settings::remove_skill(
                    p.provider,
                    &roots,
                    &p.identity,
                    &p.expected_revision,
                )
                .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&serde_json::json!({"removed": true}))
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
                if p.space_id.is_none() {
                    return Err(RpcError::BadParams(
                        "A project is required to import a session".into(),
                    ));
                }
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
                        sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
                        permission_mode: luna_proto::PermissionMode::Ask,
                        effective_permission_mode: luna_proto::PermissionMode::Ask,
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
            methods::GET_CHAT_WORKBENCH => {
                let p: luna_proto::GetChatWorkbenchRequest = parse_params(params)?;
                RpcReply::value(&self.read_chat_workbench(&p.main_chat_id)?)
            }
            methods::WATCH_CHAT_WORKBENCH => {
                let p: luna_proto::GetChatWorkbenchRequest = parse_params(params)?;
                let reader = self.chat_workbench_reader()?;
                reader.read_chat_workbench(&p.main_chat_id)?;
                let receiver = self.chat_workbenches_service()?.watch(&p.main_chat_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                // Deleting a chat must publish its pruned layout before the
                // next user action. Otherwise that action itself performs the
                // prune and conflicts with the client's still-current revision.
                let changes = futures::stream::select(
                    watch_stream(receiver).map(|_| ()),
                    watch_stream(self.workspace.watch_chats()).map(|_| ()),
                );
                let mut last_revision = None;
                Ok(RpcReply::Stream(changes.filter_map(move |_| {
                    let value = reader.read_chat_workbench(&p.main_chat_id).ok()
                        .filter(|snapshot| last_revision != Some(snapshot.revision))
                        .and_then(|snapshot| {
                            last_revision = Some(snapshot.revision);
                            serde_json::to_value(snapshot).ok()
                        });
                    futures::future::ready(value)
                }).boxed()))
            }
            methods::MUTATE_CHAT_WORKBENCH => {
                let p: luna_proto::MutateChatWorkbenchRequest = parse_params(params)?;
                RpcReply::value(&self.mutate_chat_workbench(p)?)
            }
            methods::CREATE_CHILD_TAB => {
                self.chat_workbenches_service()?;
                let p: luna_proto::CreateChildTabRequest = parse_params(params)?;
                RpcReply::value(&self.create_child_tab(p).await?)
            }
            methods::CREATE_CHILD_THREAD => Err(RpcError::Failed(
                "Child chats are created by the parent agent using create_thread. Use New side chat to fork a conversation yourself.".into(),
            )),
            methods::EXECUTE_TASK_COMMAND => {
                let command: TaskCommandParams = parse_params(params)?;
                // Child lineage, outcomes and delivery marks are engine-owned.
                // Only the parent-agent tool path creates children; clients cannot forge the outbox.
                if matches!(
                    command.intent,
                    TaskCommandIntent::CreateChildChat { .. }
                        | TaskCommandIntent::ChildOutcome { .. }
                        | TaskCommandIntent::ChildOutcomeDelivered { .. }
                ) {
                    return Err(RpcError::Failed(
                        "Child chat creation and outcomes are engine-owned; the parent agent uses create_thread"
                            .into(),
                    ));
                }
                let stop = matches!(
                    &command.intent,
                    TaskCommandIntent::Session {
                        command: SessionCommandPayload::Interrupt {},
                        ..
                    }
                )
                .then(|| (command.task_id.clone(), command.command_id.clone()));
                let receipt = self.execute_task_command(command, false).await?;
                if let Some((task_id, command_id)) = stop
                    && matches!(
                        receipt.status,
                        CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
                    )
                {
                    // Board scope extension: a ticket worker's Stop also stops
                    // that ticket's owned reviewer chats (same set as board Stop).
                    self.board_follow_chat_stop(&task_id, &command_id).await;
                }
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
            methods::GET_TASK_CLEANUP => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    task_id: String,
                }
                let p: P = parse_params(params)?;
                let status = self
                    .store
                    .task_cleanup_state(&p.task_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&status.map(|(state, detail)| {
                    serde_json::json!({
                        "state": state, "detail": detail
                    })
                }))
            }
            methods::GET_PROJECT_CLEANUP => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    project_id: String,
                }
                let p: P = parse_params(params)?;
                let status = self
                    .store
                    .project_cleanup_state(&p.project_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&status.map(|(state, detail)| {
                    serde_json::json!({
                        "state": state, "detail": detail
                    })
                }))
            }
            methods::GET_DELETION_CLEANUP_SUMMARY => {
                let (pending, failed, first_failure) = self
                    .store
                    .deletion_cleanup_summary()
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&serde_json::json!({
                    "pending": pending, "failed": failed, "firstFailure": first_failure,
                }))
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
                if outcome == luna_proto::ComposerDraftSaveOutcome::Saved
                    && let Some(task_id) = p.draft.task_id.as_deref()
                {
                    match self.store.has_appshot_activity_for_task(task_id) {
                        Ok(true) => {
                            if let Err(error) = self.refresh_appshot_history_for_task(task_id) {
                                tracing::warn!(task_id, %error, "Appshot history projection deferred after durable draft save");
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(task_id, %error, "Appshot history lookup deferred after durable draft save")
                        }
                    }
                }
                RpcReply::value(&outcome)
            }
            methods::LIST_PROMPT_STASH => RpcReply::value(&prompt_stash::list(self)?),
            methods::SAVE_PROMPT_STASH => {
                let p: SavePromptStashParams = parse_params(params)?;
                prompt_stash::save(self, &p.entry)?;
                RpcReply::value(&serde_json::json!({ "saved": true }))
            }
            methods::DELETE_PROMPT_STASH => {
                let p: DeletePromptStashParams = parse_params(params)?;
                let deleted = prompt_stash::delete(self, &p.id)?;
                RpcReply::value(&serde_json::json!({ "deleted": deleted }))
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
                let mut discarded = false;
                let mut cleared_revision = None;
                match p.outcome {
                    DraftSubmissionOutcome::Pending => {}
                    DraftSubmissionOutcome::Recoverable => self
                        .store
                        .mark_draft_submission_recoverable(
                            &self.engine_info.device_id,
                            &p.command_id,
                        )
                        .map_err(|error| RpcError::Failed(error.to_string()))?,
                    DraftSubmissionOutcome::Discarded => {
                        let task_id = self
                            .store
                            .draft_submission_task_id(&self.engine_info.device_id, &p.command_id)
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                        (discarded, cleared_revision) = self
                            .store
                            .discard_recoverable_draft_submission(
                                &self.engine_info.device_id,
                                &p.command_id,
                            )
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                        if cleared_revision.is_some()
                            && let Some(task_id) = task_id
                            && let Err(error) = self.refresh_appshot_history_for_task(&task_id)
                        {
                            tracing::warn!(task_id, %error, "Appshot history projection deferred after draft discard");
                        }
                    }
                    DraftSubmissionOutcome::Applied | DraftSubmissionOutcome::Recovered => {
                        let task_id = self
                            .store
                            .draft_submission_task_id(&self.engine_info.device_id, &p.command_id)
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                        if matches!(p.outcome, DraftSubmissionOutcome::Applied)
                            && p.queue_id.is_none()
                        {
                            self.store
                                .accept_applied_draft_submission(
                                    &self.engine_info.device_id,
                                    &p.command_id,
                                )
                                .map_err(|error| RpcError::Failed(error.to_string()))?;
                        } else {
                            self.store
                                .accept_draft_submission(
                                    &self.engine_info.device_id,
                                    &p.command_id,
                                    p.queue_id.as_deref(),
                                )
                                .map_err(|error| RpcError::Failed(error.to_string()))?;
                        }
                        if let Some(task_id) = task_id
                            && let Err(error) = self.refresh_appshot_history_for_task(&task_id)
                        {
                            tracing::warn!(task_id, %error, "Appshot history projection deferred after accepted send");
                        }
                    }
                }
                RpcReply::value(&serde_json::json!({
                    "ok": true,
                    "discarded": discarded,
                    "clearedRevision": cleared_revision,
                }))
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
            methods::GET_PROVIDER_UPDATES => RpcReply::value(
                &crate::provider_updates::check(
                    params
                        .get("force")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                )
                .await,
            ),
            methods::UPDATE_PROVIDER => {
                let harness: luna_proto::HarnessId =
                    parse_params(params.get("harness").cloned().unwrap_or_default())?;
                let installed_version = crate::provider_updates::update(harness)
                    .await
                    .map_err(RpcError::Failed)?;
                let runtime_reload_error = self
                    .sessions
                    .prepare_provider_account_change(harness)
                    .await
                    .err()
                    .map(|error| error.to_string());
                let model_refresh_error = self
                    .model_cache
                    .refresh_after_provider_update(harness)
                    .await
                    .err();
                RpcReply::value(&luna_proto::ProviderUpdateResult {
                    harness,
                    installed_version,
                    runtime_reload_error,
                    model_refresh_error,
                })
            }
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
                update_harness_enabled(&self.registry, p.harness, p.enabled)?;
                // Fresh catalog in the reply: the page repaints from it in one
                // round trip, and a refused/raced toggle self-corrects.
                RpcReply::value(&self.registry.descriptors())
            }
            methods::LIST_MODELS => {
                let p: ListModelsParams = parse_params(params)?;
                let models = self
                    .model_cache
                    .list(p.harness, p.account_id.as_deref(), p.refresh)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&models)
            }
            methods::LIST_COMPOSER_CATALOG => {
                let p: luna_proto::ComposerCatalogRequest = parse_params(params)?;
                let chat = p
                    .chat_id
                    .as_deref()
                    .map(|id| self.workspace.chat(id))
                    .transpose()
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .flatten();
                if p.chat_id.is_some() && chat.is_none() {
                    return Err(RpcError::BadParams("unknown catalog task".into()));
                }
                let requested_account = p.provider_account_id.as_deref().or_else(|| {
                    chat.as_ref()
                        .and_then(|chat| chat.config.as_ref())
                        .and_then(|config| config.provider_account_id.as_deref())
                });
                if chat
                    .as_ref()
                    .and_then(|chat| chat.config.as_ref())
                    .is_some_and(|config| config.harness != HarnessId::Codex)
                {
                    return Err(RpcError::BadParams(
                        "native integrations require Codex".into(),
                    ));
                }
                let account = self
                    .agent_accounts
                    .resolve_execution(HarnessId::Codex, requested_account)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let home = account
                    .codex_home
                    .as_deref()
                    .ok_or_else(|| RpcError::Failed("Codex home is unavailable".into()))?;
                let root = self
                    .file_search_root(&FileSearchParams {
                        query: String::new(),
                        chat_id: p.chat_id.clone(),
                        space_id: p.space_id.clone(),
                        path: p.path.clone(),
                    })
                    .await?
                    .canonicalize()
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let thread = p
                    .chat_id
                    .as_deref()
                    .and_then(|id| self.task_identity.view(id))
                    .and_then(|view| view.current_binding)
                    .filter(|binding| binding.provider_id == HarnessId::Codex)
                    .map(|binding| binding.provider_task_id)
                    .filter(|id| !id.is_empty())
                    .or_else(|| {
                        chat.as_ref()
                            .and_then(|chat| chat.harness_session_id.clone())
                    });
                let harness = self
                    .registry
                    .resolve(HarnessId::Codex)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let source = harness.composer_catalog_source();
                let scope = crate::repos::hex(&sha2::Sha256::digest(
                    format!(
                        "{:?}",
                        (
                            &self.engine_info.device_id,
                            &account.account_id,
                            &account.auth_instance_id,
                            home,
                            &root,
                            &thread,
                            &source
                        )
                    )
                    .as_bytes(),
                ));
                // Live thread policy can change without a global plugin notification.
                // Query the loaded runtime each open; only threadless catalogs use TTL reuse.
                if !p.refresh
                    && thread.is_none()
                    && let Some(catalog) = self
                        .composer_catalog
                        .lock()
                        .map_err(|_| RpcError::Failed("catalog cache unavailable".into()))?
                        .fresh(&scope)
                {
                    return RpcReply::value(&catalog);
                }
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    harness.composer_catalog(
                        &root,
                        home,
                        account.codex_file_auth,
                        thread.as_deref(),
                    ),
                )
                .await;
                // Never publish a response after account or native executable identity changed.
                if self
                    .agent_accounts
                    .resolve_execution(HarnessId::Codex, requested_account)
                    .ok()
                    .as_ref()
                    != Some(&account)
                    || source != harness.composer_catalog_source()
                {
                    return Err(RpcError::Failed(
                        "Execution account or Codex changed during discovery; retry".into(),
                    ));
                }
                let current_root = self
                    .file_search_root(&FileSearchParams {
                        query: String::new(),
                        chat_id: p.chat_id.clone(),
                        space_id: p.space_id.clone(),
                        path: p.path.clone(),
                    })
                    .await?
                    .canonicalize()
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let current_chat = p
                    .chat_id
                    .as_deref()
                    .map(|id| self.workspace.chat(id))
                    .transpose()
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .flatten();
                let current_thread = p
                    .chat_id
                    .as_deref()
                    .and_then(|id| self.task_identity.view(id))
                    .and_then(|view| view.current_binding)
                    .filter(|binding| binding.provider_id == HarnessId::Codex)
                    .map(|binding| binding.provider_task_id)
                    .filter(|id| !id.is_empty())
                    .or_else(|| {
                        current_chat
                            .as_ref()
                            .and_then(|chat| chat.harness_session_id.clone())
                    });
                if current_root != root
                    || current_thread != thread
                    || (p.chat_id.is_some() && current_chat.is_none())
                {
                    return Err(RpcError::Failed(
                        "Execution workspace or native thread changed during discovery; retry"
                            .into(),
                    ));
                }
                let catalog = match result {
                    Ok(Ok(catalog)) => catalog,
                    _ => crate::composer_catalog::unavailable(),
                };
                let catalog = self
                    .composer_catalog
                    .lock()
                    .map_err(|_| RpcError::Failed("catalog cache unavailable".into()))?
                    .update(scope, catalog);
                RpcReply::value(&catalog)
            }
            methods::LIST_COMMANDS => {
                // Same shape as ListModels: resolve the installed provider,
                // then ask its own command discovery surface. The mock uses
                // the trait's empty default. Discovery runs under the scope the
                // chat/space's next run launches with (G1, E23).
                let p: crate::command_scope::ListCommandsParams = parse_params(params)?;
                let harness = self
                    .registry
                    .resolve(p.harness)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let scope = crate::command_scope::resolve(
                    &p,
                    &self.workspace,
                    &self.workspace_files,
                    self.doc_host.device_id(),
                    &self.agent_accounts,
                )
                .await?;
                let commands = harness
                    .commands_in(&scope)
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
                let p: TranscriptWatchParams = parse_params(params)?;
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
                let rx = handle.watch_messages();
                let stream = if p.transcript_resume {
                    doc_messages_stream_with_resume(
                        rx, handle.doc_arc(), true, p.known_transcript_token,
                    )
                } else {
                    doc_messages_stream(rx, handle.doc_arc())
                };
                Ok(RpcReply::Stream(stream))
            }
            methods::WATCH_NATIVE_TASKS => {
                let p: ChatParams = parse_params(params)?;
                let handle = self
                    .doc_host
                    .open(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                Ok(RpcReply::Stream(crate::native_tasks::watch_stream(
                    handle.watch_messages(),
                    handle.doc_arc(),
                )))
            }
            methods::LIST_CLAUDE_EXTERNAL_SESSIONS => {
                RpcReply::value(&crate::native_tasks::list_claude_external_sessions().await)
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
                        input_items: p.input_items,
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
                            input_items: p.input_items,
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
                fn room_json(s: &luna_sync::RoomStatsSnapshot) -> serde_json::Value {
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
                fn chat2_json(s: &luna_sync::ChatStatsSnapshot) -> serde_json::Value {
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
                let p: luna_proto::WatchPreviewsParams = parse_params(params)?;
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
                let side_chats = self.side_chats.clone();
                let workspace = self.workspace.clone();
                let task_identity = self.task_identity.clone();
                let sessions = self.sessions.clone();
                let changes = watch_stream(self.workspace.watch_chats()).boxed();
                let execution_workspace = workspace.clone();
                let changes = futures::stream::select(changes, watch_stream(sessions.watch_sessions()).map(move |_| {
                    serde_json::to_value(execution_workspace.watch_chats().borrow().clone()).unwrap_or_default()
                })).boxed();
                let changes = if let Some(service) = &side_chats {
                    let workspace = workspace.clone();
                    futures::stream::select(changes, watch_stream(service.watch_changes()).map(move |_| {
                        serde_json::to_value(workspace.watch_chats().borrow().clone()).unwrap_or_default()
                    })).boxed()
                } else { changes };
                Ok(RpcReply::Stream(changes.map(move |mut value| {
                    // Fork origin augments the ordinary project row. A source
                    // disappearing never hides a surviving fork.
                    let side_states = side_chats.as_ref().and_then(|service| service.published_states().ok()).unwrap_or_default();
                    if let Some(items) = value.as_array_mut() {
                        items.retain_mut(|chat| {
                            let Some(id) = chat.get("id").and_then(|id| id.as_str()).map(str::to_owned) else { return false };
                            if let Ok(Some(execution)) = sessions.execution_summary(&id)
                                && let Some(object) = chat.as_object_mut() {
                                object.insert("execution".into(), serde_json::to_value(execution).unwrap_or_default());
                            }
                            // Native history forks are ordinary tasks, too. Their
                            // immutable provenance is independent of the legacy
                            // side-chat service and remains valid after source
                            // deletion or a later provider/context boundary.
                            if let Some(source) = task_identity.view(&id)
                                .filter(|view| !view.task.tombstoned)
                                .and_then(|view| view.task.lineage.source_task_id)
                                .filter(|source| source != &id)
                                && let Some(object) = chat.as_object_mut() {
                                object.insert("forkSourceTaskId".into(), source.into());
                            }
                            if let Some((parent, state, context_mode)) = side_states.get(&id) {
                                if let Some(object) = chat.as_object_mut() {
                                    object.insert("sideChatParentId".into(), parent.clone().into());
                                    object.insert("sideChatState".into(), state.clone().into());
                                    object.insert("sideChatContextMode".into(), context_mode.clone().into());
                                }
                                true
                            } else {
                                if let Some(parent) = workspace.side_chat_parent(&id)
                                    && let Some(object) = chat.as_object_mut() {
                                    object.insert("sideChatParentId".into(), parent.into());
                                    object.insert("sideChatState".into(), "active".into());
                                }
                                chat.get("spaceId").and_then(|id| id.as_str()).is_some()
                            }
                        });
                    }
                    value
                }).boxed()))
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
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    chat_id: String,
                    expected_checkout_id: Option<String>,
                }
                let p: P = parse_params(params)?;
                let recorded = self
                    .trusted_git_checkout(&p.chat_id, &p.cwd, p.expected_checkout_id.as_deref())
                    .await?;
                let preview = self
                    .git_actions
                    .preview(&p.cwd)
                    .await
                    .map_err(RpcError::Failed)?;
                self.trusted_git_checkout(&p.chat_id, &p.cwd, Some(&recorded))
                    .await?;
                if preview.checkout_id != recorded {
                    return Err(RpcError::Failed(
                        "Task checkout changed; refresh Git".into(),
                    ));
                }
                RpcReply::value(&preview)
            }
            methods::EXECUTE_GIT_ACTION => {
                let chat_id = Self::git_request_chat_id(&params)?.to_owned();
                let request: luna_proto::GitActionRequest = parse_params(params)?;
                self.trusted_git_checkout(&chat_id, &request.cwd, Some(&request.checkout_id))
                    .await?;
                let receipt = self
                    .git_actions
                    .execute(request)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&receipt)
            }
            methods::GENERATE_GIT_COMMIT_MESSAGE => {
                let chat_id = Self::git_request_chat_id(&params)?.to_owned();
                let request: luna_proto::GitMessageRequest = parse_params(params)?;
                self.trusted_git_checkout(&chat_id, &request.cwd, Some(&request.checkout_id))
                    .await?;
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
            methods::GET_GITHUB_PR_PREVIEW | methods::GET_GITHUB_PR_DETAILS => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    chat_id: String,
                    expected_checkout_id: Option<String>,
                    number: Option<u64>,
                }
                let p: P = parse_params(params)?;
                let recorded = self
                    .trusted_git_checkout(&p.chat_id, &p.cwd, p.expected_checkout_id.as_deref())
                    .await?;
                let root = self.change_request_root(&p.cwd).await?;
                let cwd = root.to_string_lossy();
                let service = self
                    .github_pr
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("GitHub PR service is unavailable".into()))?;
                if method == methods::GET_GITHUB_PR_PREVIEW {
                    let preview = service.preview(&cwd).await.map_err(RpcError::Failed)?;
                    self.trusted_git_checkout(&p.chat_id, &p.cwd, Some(&recorded))
                        .await?;
                    if preview.checkout.checkout_id != recorded {
                        return Err(RpcError::Failed(
                            "Task checkout changed; refresh Git".into(),
                        ));
                    }
                    RpcReply::value(&preview)
                } else {
                    let number = p
                        .number
                        .ok_or_else(|| RpcError::Failed("PR number is required".into()))?;
                    let details = service
                        .details(&cwd, number)
                        .await
                        .map_err(RpcError::Failed)?;
                    self.trusted_git_checkout(&p.chat_id, &p.cwd, Some(&recorded))
                        .await?;
                    RpcReply::value(&details)
                }
            }
            methods::CREATE_GITHUB_DRAFT_PR => {
                let chat_id = Self::git_request_chat_id(&params)?.to_owned();
                let request: luna_proto::GitPrCreateRequest = parse_params(params)?;
                self.trusted_git_checkout(
                    &chat_id,
                    &request.preview.checkout.cwd,
                    Some(&request.preview.checkout.checkout_id),
                )
                .await?;
                self.change_request_root(&request.preview.checkout.cwd)
                    .await?;
                let service = self
                    .github_pr
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("GitHub PR service is unavailable".into()))?;
                RpcReply::value(&service.create(request).await.map_err(RpcError::Failed)?)
            }
            methods::GET_GIT_PR_STATUS => {
                Box::pin(async move {
                    let chat_id = Self::git_request_chat_id(&params)?.to_owned();
                    let mut request: luna_proto::GitPrStatusRequest = parse_params(params)?;
                    let original_cwd = request.cwd.clone();
                    let recorded = self
                        .trusted_git_checkout(&chat_id, &original_cwd, Some(&request.checkout_id))
                        .await?;
                    let root = self.change_request_root(&request.cwd).await?;
                    request.cwd = root.to_string_lossy().into_owned();
                    let status = self
                        .git_actions
                        .pr_status(request)
                        .await
                        .map_err(RpcError::Failed)?;
                    self.trusted_git_checkout(&chat_id, &original_cwd, Some(&recorded))
                        .await?;
                    RpcReply::value(&status)
                })
                .await
            }
            methods::LIST_GIT_CHECKPOINTS => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    chat_id: String,
                    cwd: String,
                }
                let p: P = parse_params(params)?;
                self.validate_checkpoint_task(&p.chat_id, &p.cwd)?;
                let busy = self.checkpoint_checkout_busy(&p.chat_id, &p.cwd).await?;
                let list = self
                    .checkpoint_service()?
                    .list(&p.chat_id, &p.cwd, busy)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&list)
            }
            methods::DIFF_GIT_CHECKPOINT => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    chat_id: String,
                    cwd: String,
                    turn_id: String,
                }
                let p: P = parse_params(params)?;
                self.validate_checkpoint_task(&p.chat_id, &p.cwd)?;
                let diff = self
                    .checkpoint_service()?
                    .diff(&p.chat_id, &p.cwd, &p.turn_id)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&diff)
            }
            methods::PREVIEW_GIT_CHECKPOINT_RESTORE => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    chat_id: String,
                    cwd: String,
                    turn_id: String,
                    point: String,
                }
                let p: P = parse_params(params)?;
                self.validate_checkpoint_task(&p.chat_id, &p.cwd)?;
                let busy = self.checkpoint_checkout_busy(&p.chat_id, &p.cwd).await?;
                let preview = self
                    .checkpoint_service()?
                    .preview_restore(&p.chat_id, &p.cwd, &p.turn_id, &p.point, busy)
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&preview)
            }
            methods::RESTORE_GIT_CHECKPOINT => {
                let request: luna_proto::GitCheckpointRestoreRequest = parse_params(params)?;
                let _task_lane = self.doc_host.task_command_gate(&request.chat_id).await;
                self.validate_checkpoint_task(&request.chat_id, &request.cwd)?;
                let busy = self
                    .checkpoint_checkout_busy(&request.chat_id, &request.cwd)
                    .await?;
                let chat_id = request.chat_id.clone();
                let receipt = self
                    .checkpoint_service()?
                    .restore(request, busy, |root| {
                        self.checkpoint_owner_overlap_now(&chat_id, root)
                    })
                    .await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&receipt)
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
                    RpcReply::value(&luna_proto::CheckoutDiff {
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
                    let p: luna_proto::GetCheckoutFileDiffTextRequest = parse_params(params)?;
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
                    let stale = || luna_proto::CheckoutFileDiffText {
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
                    RpcReply::value(&luna_proto::CheckoutFileDiffText {
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
                RpcReply::value(&luna_proto::DriveListing { drives })
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
                let request: luna_proto::ListWorkspaceDirectoryRequest = parse_params(params)?;
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
                let request: luna_proto::SearchWorkspaceFilesRequest = parse_params(params)?;
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
                let request: luna_proto::ReadWorkspaceImageRequest = parse_params(params)?;
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
                let request: luna_proto::ReadWorkspaceFileRequest = parse_params(params)?;
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
                let request: luna_proto::WriteWorkspaceFileRequest = parse_params(params)?;
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
                let request: luna_proto::WatchWorkspaceFilesRequest = parse_params(params)?;
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
            methods::GET_PROJECT_COMMANDS
            | methods::SAVE_PROJECT_COMMANDS
            | methods::PROJECT_COMMAND_STATUS
            | methods::RUN_PROJECT_COMMAND
            | methods::STOP_PROJECT_COMMAND
            | methods::RETRY_PROJECT_COMMAND => {
                let commands = self
                    .project_commands
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("Project commands unavailable".into()))?;
                let p: ProjectCommandsParams = parse_params(params)?;
                let failed = |e: crate::EngineError| RpcError::Failed(e.to_string());
                match method {
                    methods::GET_PROJECT_COMMANDS => {
                        RpcReply::value(&commands.configuration(&p.project_id).map_err(failed)?)
                    }
                    methods::SAVE_PROJECT_COMMANDS => {
                        commands
                            .configure(
                                &p.project_id,
                                &p.expected_actions.ok_or_else(|| RpcError::Failed("Missing expectedActions snapshot; reload project commands before saving".into()))?,
                                p.actions
                                    .ok_or_else(|| RpcError::Failed("Missing actions".into()))?,
                            )
                            .map_err(failed)?;
                        RpcReply::value(&commands.configuration(&p.project_id).map_err(failed)?)
                    }
                    methods::PROJECT_COMMAND_STATUS => {
                        RpcReply::value(&commands.snapshot(&p.chat_id).await.map_err(failed)?)
                    }
                    methods::RUN_PROJECT_COMMAND => RpcReply::value(
                        &commands
                            .run(&p.chat_id, &p.action_id, &p.request_id)
                            .await
                            .map_err(failed)?,
                    ),
                    methods::STOP_PROJECT_COMMAND => RpcReply::value(
                        &commands.stop(&p.chat_id, &p.run_id).await.map_err(failed)?,
                    ),
                    _ => RpcReply::value(
                        &commands
                            .retry(&p.chat_id, &p.run_id)
                            .await
                            .map_err(failed)?,
                    ),
                }
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
                let usage_scope = p.harness.is_none().then(|| self.usage_cache.history_scope());
                let force_direct = p.force_usage.unwrap_or(false) && !p.background;
                let mut snapshot = match p.harness {
                    Some(harness) => self.agent_accounts.list_harness(harness, force_direct).await,
                    None => self.agent_accounts.list(force_direct).await,
                }.map_err(|e| RpcError::Failed(e.to_string()))?;
                // A provider-scoped Codex listing must not touch Claude.
                if p.harness.is_none() {
                    if !force_direct { self.usage_cache.queue_accounts(p.force_usage.unwrap_or(false)); }
                    let usage = self.usage_cache.read(luna_proto::UsageReadRequest {
                        query: UsageRequest { days: 7, period_starts_ms: vec![], until_ms: None, include_limits: true, price_overrides: Default::default() },
                        include_history: false, background: p.background || !p.force_usage.unwrap_or(false), refresh: p.force_usage.unwrap_or(false),
                    }).await.map_err(RpcError::Failed)?;
                    if usage_scope.as_deref() != Some(usage.scope.as_str()) {
                        return Err(RpcError::Failed("Provider account selection changed during account usage refresh".into()));
                    }
                    for account in snapshot.accounts.iter_mut().filter(|a| a.active) {
                        let provider = match account.harness { HarnessId::Codex => luna_proto::UsageProvider::Codex, HarnessId::ClaudeCode => luna_proto::UsageProvider::Claude, _ => continue };
                        if let Some(limit) = usage.snapshot.limits.iter().find(|l| l.provider == provider) {
                            account.usage_windows = limit.windows.iter().map(|w| luna_proto::AgentUsageWindow {
                                label: w.label.clone(), used_fraction: w.used_percent / 100.0,
                                resets_at: w.resets_at_ms.and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis),
                            }).collect();
                            if limit.plan.is_some() { account.plan_label = limit.plan.clone(); }
                            if limit.is_fresh(chrono::Utc::now().timestamp_millis()) {
                                account.authentication = luna_proto::AgentAuthenticationState::Authenticated;
                                if account.email.is_none() { account.email = limit.account_label.clone().filter(|label| label.contains('@')); }
                            }
                        }
                    }
                    if snapshot.accounts.iter().any(|account| account.harness == HarnessId::Codex && account.active && account.authentication == luna_proto::AgentAuthenticationState::Authenticated) {
                        snapshot.warnings.retain(|warning| !warning.message.starts_with("Codex may keep the native login in its provider-managed OS credential store."));
                    }
                }
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
                let local = match p.blob_ref.split_once('/') {
                    Some((chat_id, part_id)) => {
                        let sessions = self.sessions.clone();
                        let chat_id = chat_id.to_owned();
                        let part_id = part_id.to_owned();
                        tokio::task::spawn_blocking(move || {
                            sessions.tool_output(&chat_id, &part_id)
                        })
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                    }
                    _ => None,
                };
                let text = match local {
                    Some(text) => text,
                    None => self
                        .doc_host
                        .fetch_tool_blob(&p.blob_ref)
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?,
                };
                RpcReply::value(&serde_json::json!({ "text": text }))
            }
            methods::MCP_ELICITATION_URL => {
                let p: McpElicitationUrlParams = parse_params(params)?;
                let url = self
                    .sessions
                    .mcp_elicitation_url(&p.chat_id, &p.request_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn checkpoint_fixture_git(root: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn checkpoint_restore_owner_gate_catches_parent_and_nested_tasks() {
        let root = std::path::Path::new("/fixture/managed/task");
        assert!(restore_owner_paths_overlap(root, root));
        assert!(restore_owner_paths_overlap(
            root,
            std::path::Path::new("/fixture/managed")
        ));
        assert!(restore_owner_paths_overlap(
            root,
            std::path::Path::new("/fixture/managed/task/nested")
        ));
        assert!(!restore_owner_paths_overlap(
            root,
            std::path::Path::new("/fixture/managed/sibling")
        ));
    }

    #[tokio::test]
    async fn checkpoint_restore_rechecks_deleted_or_moved_target_before_file_mutation() {
        for move_target in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            checkpoint_fixture_git(&repo, &["init", "-q", "-b", "main"]);
            checkpoint_fixture_git(&repo, &["config", "user.name", "LUNA Fixture"]);
            checkpoint_fixture_git(&repo, &["config", "user.email", "fixture@example.invalid"]);
            std::fs::write(repo.join("base.txt"), "base\n").unwrap();
            checkpoint_fixture_git(&repo, &["add", "base.txt"]);
            checkpoint_fixture_git(&repo, &["commit", "-q", "-m", "base"]);
            let managed = root.path().join("managed");
            let worktree = managed.join("repo").join("turn");
            std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
            checkpoint_fixture_git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "turn",
                    worktree.to_str().unwrap(),
                ],
            );
            let accounts = crate::agent_accounts::AgentAccountsConfig {
                data_dir: root.path().join("synthetic-provider-state"),
                claude_config_dir: root.path().join("synthetic-claude"),
                claude_config_file: root.path().join("synthetic-claude.json"),
                codex_home: root.path().join("synthetic-codex"),
            };
            let core = crate::EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                crate::EngineProfile::development(&root.path().join("synthetic-engine"), "synthetic-org", "synthetic-user"),
                Arc::new(crate::registry::HarnessRegistry::new()), HarnessId::Mock, None, accounts,
            ).unwrap();
            let cwd = worktree.to_string_lossy().into_owned();
            core.workspace
                .create_chat("task", None, Some(&core.device_id), None, Some(cwd.clone()))
                .unwrap();
            let rpc = core.rpc_service();
            assert!(
                !rpc.checkpoint_owner_overlap_now("task", &worktree.canonicalize().unwrap()),
                "chat={:?} identity={:?}",
                core.workspace.chat("task").unwrap(),
                core.task_identity.view("task")
            );
            let repos = crate::Repos::with_worktrees_root(
                &root.path().join("checkpoint-data"),
                "fixture",
                managed,
            );
            let service = crate::git_checkpoints::GitCheckpoints::new(repos);
            service.start("task", "fixture-run", &cwd).await;
            service.end("task", "fixture-run", "completed").await;
            let turn_id = service.list("task", &cwd, false).await.unwrap().checkpoints[0]
                .turn_id
                .clone();
            std::fs::write(worktree.join("base.txt"), "current edit\n").unwrap();
            let preview = service
                .preview_restore("task", &cwd, &turn_id, "before", false)
                .await
                .unwrap();
            let before_status = checkpoint_fixture_git(&worktree, &["status", "--porcelain"]);
            let before_index = checkpoint_fixture_git(&worktree, &["write-tree"]);
            let before_head = checkpoint_fixture_git(&worktree, &["rev-parse", "HEAD"]);
            let moved = root.path().join("other-workspace");
            std::fs::create_dir(&moved).unwrap();
            let probes = AtomicUsize::new(0);
            let error = service
                .restore(
                    luna_proto::GitCheckpointRestoreRequest {
                        chat_id: "task".into(),
                        cwd: cwd.clone(),
                        turn_id,
                        point: "before".into(),
                        expected_token: preview.token,
                    },
                    false,
                    |checkout| {
                        if probes.fetch_add(1, Ordering::SeqCst) == 1 {
                            if move_target {
                                core.workspace
                                    .set_chat_cwd("task", moved.to_str().unwrap())
                                    .unwrap();
                            } else {
                                core.workspace.delete_chat("task").unwrap();
                            }
                        }
                        rpc.checkpoint_owner_overlap_now("task", checkout)
                    },
                )
                .await
                .unwrap_err();
            assert!(error.contains("one task owner"), "{error}");
            assert!(probes.load(Ordering::SeqCst) >= 2);
            assert_eq!(
                checkpoint_fixture_git(&worktree, &["status", "--porcelain"]),
                before_status
            );
            assert_eq!(
                checkpoint_fixture_git(&worktree, &["write-tree"]),
                before_index
            );
            assert_eq!(
                checkpoint_fixture_git(&worktree, &["rev-parse", "HEAD"]),
                before_head
            );
            assert_eq!(
                std::fs::read_to_string(worktree.join("base.txt")).unwrap(),
                "current edit\n"
            );
            core.shutdown().await;
        }
    }

    #[tokio::test]
    async fn project_cleanup_waits_for_authoritative_registry_then_continues() {
        let root = tempfile::tempdir().unwrap();
        let accounts = crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.path().join("synthetic-provider-state"),
            claude_config_dir: root.path().join("synthetic-claude"),
            claude_config_file: root.path().join("synthetic-claude.json"),
            codex_home: root.path().join("synthetic-codex"),
        };
        let core = crate::EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            crate::EngineProfile::development(&root.path().join("synthetic-engine"), "synthetic-org", "synthetic-user"),
            Arc::new(crate::registry::HarnessRegistry::new()), HarnessId::Mock, None, accounts,
        ).unwrap();
        let folder = root.path().join("workspace");
        std::fs::create_dir_all(&folder).unwrap();
        core.workspace
            .create_space(
                "delayed-project",
                &core.device_id,
                folder.to_str().unwrap(),
                None,
                false,
            )
            .unwrap();
        core.workspace
            .create_chat("delayed-task", Some("delayed-project"), None, None, None)
            .unwrap();
        let rpc = core.rpc_service();
        core.store
            .prepare_project_cleanup("delayed-project", &["delayed-task".into()], "prior-boot")
            .unwrap();
        core.store
            .prepare_task_cleanup(&core.device_id, "delayed-task", "prior-boot")
            .unwrap();
        core.store
            .save_snapshot("delayed-task", b"synthetic-snapshot")
            .unwrap();
        core.workspace.delete_space("delayed-project").unwrap();
        core.workspace.flush_checked().unwrap();

        assert_eq!(
            rpc.resume_pending_project_cleanups_with_readiness("", false)
                .1,
            0
        );
        assert!(core.store.has_snapshot("delayed-task").unwrap());
        assert_eq!(
            core.store
                .project_cleanup_state("delayed-project")
                .unwrap()
                .unwrap()
                .0,
            "pending"
        );
        assert_eq!(
            rpc.resume_pending_project_cleanups_with_readiness("", true)
                .1,
            1
        );
        assert!(!core.store.has_snapshot("delayed-task").unwrap());
        assert_eq!(
            core.store
                .project_cleanup_state("delayed-project")
                .unwrap()
                .unwrap()
                .0,
            "completed"
        );
        core.shutdown().await;
    }

    #[test]
    fn disabled_agent_usage_is_removed_from_all_snapshot_sections() {
        assert_eq!(
            EngineRpc::usage_providers_for(&[HarnessId::Codex, HarnessId::Hermes]),
            vec![luna_proto::UsageProvider::Codex]
        );
        let mut snapshot = crate::usage::snapshot(UsageRequest {
            days: 7,
            period_starts_ms: vec![],
            until_ms: None,
            include_limits: true,
            price_overrides: std::collections::HashMap::new(),
        });
        assert_eq!(snapshot.summary.sources.len(), 3);
        assert_eq!(snapshot.limits.len(), 3);
        snapshot.summary.buckets.push(luna_proto::UsageBucket {
            start_ms: 0,
            provider: luna_proto::UsageProvider::Claude,
            model: "synthetic-model".into(),
            tokens: luna_proto::UsageTokens::default(),
            records: 1,
            sessions: 1,
            api_equivalent_cost_usd: None,
            known_cost_usd: None,
            unpriced_records: None,
            cache_savings_usd: None,
            cost_quality: luna_proto::CostQuality::Unpriced,
        });
        EngineRpc::retain_enabled_usage(&mut snapshot, &[luna_proto::UsageProvider::Codex]);
        assert!(
            snapshot
                .summary
                .sources
                .iter()
                .all(|source| source.provider == luna_proto::UsageProvider::Codex)
        );
        assert!(snapshot.summary.buckets.is_empty());
        assert!(
            snapshot
                .limits
                .iter()
                .all(|limit| limit.provider == luna_proto::UsageProvider::Codex)
        );
    }

    #[tokio::test]
    async fn control_evidence_backend_uses_only_local_synthetic_task_state() {
        let root = tempfile::tempdir().unwrap();
        let accounts = crate::agent_accounts::AgentAccountsConfig {
            data_dir: root.path().join("synthetic-provider-state"),
            claude_config_dir: root.path().join("synthetic-claude"),
            claude_config_file: root.path().join("synthetic-claude.json"),
            codex_home: root.path().join("synthetic-codex"),
        };
        accounts.disable_claude_keychain_fallback().unwrap();
        let core = crate::EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            crate::EngineProfile::development(
                &root.path().join("synthetic-engine"),
                "synthetic-org",
                "synthetic-user",
            ),
            Arc::new(crate::registry::HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
        .unwrap();
        let caller_id = uuid::Uuid::new_v4().to_string();
        let target_id = uuid::Uuid::new_v4().to_string();
        let foreign_id = uuid::Uuid::new_v4().to_string();
        for project in ["project-a", "project-b"] {
            let path = root.path().join(project);
            std::fs::create_dir_all(&path).unwrap();
            core.workspace
                .create_space(
                    project,
                    &core.device_id,
                    &path.display().to_string(),
                    None,
                    false,
                )
                .unwrap();
        }
        for (task_id, project) in [
            (&caller_id, "project-a"),
            (&target_id, "project-a"),
            (&foreign_id, "project-b"),
        ] {
            core.workspace
                .create_chat(task_id, Some(project), None, None, None)
                .unwrap();
            core.task_identity
                .register_new_task(
                    task_id,
                    Some(project),
                    None,
                    root.path().join(project).to_str(),
                    TaskLineage::default(),
                )
                .unwrap();
        }
        let rpc = core.rpc_service();
        let authority = CrossTaskAuthority {
            caller_task_id: caller_id,
            local_environment_id: core.device_id.clone(),
            visible_project_ids: HashSet::from(["project-a".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.read".into()]),
            explicit_user_request: false,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        };
        let snapshot =
            CrossTaskBackend::control_target_snapshot(rpc.as_ref(), &target_id, &authority)
                .await
                .unwrap();
        assert_eq!(snapshot.task_id, target_id);
        assert_eq!(
            CrossTaskBackend::control_command_receipt(
                rpc.as_ref(),
                &target_id,
                "synthetic-unknown",
                &authority,
            )
            .await
            .unwrap(),
            None
        );
        for task_id in [&foreign_id, "missing-task"] {
            assert_eq!(
                CrossTaskBackend::control_target_snapshot(rpc.as_ref(), task_id, &authority).await,
                Err(CrossTaskError::TargetScope)
            );
            assert_eq!(
                CrossTaskBackend::control_command_receipt(
                    rpc.as_ref(),
                    task_id,
                    "synthetic-unknown",
                    &authority,
                )
                .await,
                Err(CrossTaskError::TargetScope)
            );
        }
        let mut wrong_environment = authority.clone();
        wrong_environment.local_environment_id = "remote".into();
        assert_eq!(
            CrossTaskBackend::control_target_snapshot(
                rpc.as_ref(),
                &target_id,
                &wrong_environment,
            )
            .await,
            Err(CrossTaskError::TargetScope)
        );
        drop(rpc);
        core.shutdown().await;
    }

    #[test]
    fn control_evidence_scope_requires_same_local_managed_project() {
        let authority = CrossTaskAuthority {
            caller_task_id: "caller".into(),
            local_environment_id: "local".into(),
            visible_project_ids: HashSet::from(["project-a".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.read".into()]),
            explicit_user_request: false,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        };
        let summary = |task_id: &str, project: Option<&str>, environment: &str| {
            luna_proto::CrossTaskSummary {
                agent: None,
                task_id: task_id.into(),
                project_id: project.map(str::to_owned),
                environment_id: environment.into(),
                title: "Synthetic task".into(),
                status: "idle".into(),
                archived: false,
                updated_at_ms: 1,
            }
        };
        let caller = summary("caller", Some("project-a"), "local");
        let target = summary("target", Some("project-a"), "local");
        assert_eq!(
            require_local_control_evidence_scope(&caller, &target, "target", &authority, "local"),
            Ok(())
        );
        for foreign in [
            summary("target", Some("project-b"), "local"),
            summary("target", Some("project-a"), "remote"),
            summary("target", None, "local"),
        ] {
            assert_eq!(
                require_local_control_evidence_scope(
                    &caller, &foreign, "target", &authority, "local"
                ),
                Err(CrossTaskError::TargetScope)
            );
        }
        assert_eq!(
            require_local_control_evidence_scope(
                &caller,
                &summary("wrong-id", Some("project-a"), "local"),
                "target",
                &authority,
                "local"
            ),
            Err(CrossTaskError::InvalidBackendResult)
        );
        let mut no_read = authority.clone();
        no_read.granted_scopes.clear();
        assert_eq!(
            require_local_control_evidence_scope(&caller, &target, "target", &no_read, "local"),
            Err(CrossTaskError::Scope)
        );
    }

    #[test]
    fn control_receipt_readback_rejects_wrong_command_or_provenance() {
        let authority = CrossTaskAuthority {
            caller_task_id: "caller".into(),
            local_environment_id: "local".into(),
            visible_project_ids: HashSet::from(["project-a".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.read".into()]),
            explicit_user_request: false,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        };
        let receipt = CommandReceipt {
            command_id: "command-a".into(),
            task_id: "target".into(),
            intent: "stop".into(),
            payload_hash: "synthetic-hash".into(),
            status: CommandReceiptStatus::Pending,
            snapshot: TaskCommandSnapshot {
                task_id: "target".into(),
                ..Default::default()
            },
            event_sequence: None,
            actor_id: "luna-tool:caller".into(),
            client_id: "luna-cross-task".into(),
            client_session_id: "caller".into(),
            execution_environment: Some("local".into()),
            correlation_id: None,
            reason: None,
            recovery_action: None,
            accepted_at_ms: 1,
            committed_at_ms: None,
            observed_at_ms: 1,
        };
        assert_eq!(
            validate_local_control_receipt(&receipt, "target", "command-a", &authority, "local"),
            Ok(())
        );
        for case in 0..8 {
            let mut wrong = receipt.clone();
            match case {
                0 => wrong.command_id = "different".into(),
                1 => wrong.task_id = "different".into(),
                2 => wrong.snapshot.task_id = "different".into(),
                3 => wrong.execution_environment = Some("remote".into()),
                4 => wrong.client_id = "different".into(),
                5 => wrong.actor_id = "luna-tool:different".into(),
                6 => wrong.client_session_id = "different".into(),
                7 => wrong.intent = "queueMessage".into(),
                _ => unreachable!(),
            }
            assert_eq!(
                validate_local_control_receipt(&wrong, "target", "command-a", &authority, "local"),
                Err(CrossTaskError::InvalidBackendResult),
                "case {case}"
            );
        }
    }

    #[test]
    fn cross_task_queue_uses_one_stable_id_and_waits_for_run_end() {
        let authority = CrossTaskAuthority {
            caller_task_id: "caller-task".into(),
            local_environment_id: "synthetic-environment".into(),
            visible_project_ids: HashSet::from(["synthetic-project".into()]),
            allow_projectless: false,
            granted_scopes: HashSet::from(["threads.send".into()]),
            explicit_user_request: true,
            delivery: luna_proto::RuntimeDelivery::CodexDynamicTools,
        };
        let request = luna_proto::CrossTaskSendRequest {
            task_id: "target-task".into(),
            message: "synthetic follow-up".into(),
            mode: luna_proto::CrossTaskSendMode::Queue,
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
        let ordinary: IntegrationInventoryParams =
            serde_json::from_value(serde_json::json!({"provider":"codex"})).unwrap();
        assert!(
            !ordinary.probe_provider,
            "ordinary Settings reads remain filesystem-only"
        );
        let refresh: IntegrationInventoryParams =
            serde_json::from_value(serde_json::json!({"provider":"codex","probeProvider":true}))
                .unwrap();
        assert!(refresh.probe_provider);
    }

    fn move_test_space(path: &str) -> luna_proto::Space {
        luna_proto::Space {
            execution_defaults: Default::default(),
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
            child_thread: None,
            id: "task".into(),
            device_id: "device".into(),
            title: Some("Task".into()),
            archived: false,
            settlement_override: None,
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
        use luna_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(luna_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: luna_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("replay-chat").unwrap();
        let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "replay-chat")
            .with_handle(Arc::downgrade(&handle));
        let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        let first: luna_doc::TranscriptUpdate =
            serde_json::from_value(stream.next().await.unwrap()).unwrap();
        assert!(first.replay_baseline.unwrap().entries.is_empty());

        let source = luna_doc::SessionDoc::init("replay-chat").unwrap();
        let append = |id: &str| {
            source
                .push_message(&luna_doc::SessionMessageEntry {
                    id: id.into(),
                    role: luna_doc::MessageRole::Assistant,
                    parts: vec![luna_doc::MessagePart::Text {
                        id: "text".into(),
                        text: id.into(),
                    }],
                    created_at: 0,
                    device_id: "writer".into(),
                    status: Some(luna_doc::MessageStatus::Streaming),
                    continuation_of: None,
                })
                .unwrap()
        };
        append("cached");
        sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
            .unwrap();
        let checkpoint: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        let update: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        luna_doc::apply_transcript_frame(&mut entries, checkpoint.frame).unwrap();
        luna_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
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
        let update: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        let opening: luna_doc::TranscriptUpdate =
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
        use luna_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(luna_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "host".into(),
                default_harness: luna_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("interleaved").unwrap();
        let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "interleaved")
            .with_handle(Arc::downgrade(&handle));
        let mut stream = doc_messages_stream(handle.watch_messages(), handle.doc_arc());
        stream.next().await.unwrap();
        let source = luna_doc::SessionDoc::init("interleaved").unwrap();
        sink.apply_checkpoint(&source.export_snapshot().unwrap(), 0)
            .unwrap();
        let entry = |id: &str| luna_doc::SessionMessageEntry {
            id: id.into(),
            role: luna_doc::MessageRole::Assistant,
            parts: vec![luna_doc::MessagePart::Text {
                id: "text".into(),
                text: id.into(),
            }],
            created_at: 0,
            device_id: "host".into(),
            status: Some(luna_doc::MessageStatus::Streaming),
            continuation_of: None,
        };
        handle.doc().push_message(&entry("local-before")).unwrap();
        source.update_context_usage(Some(10), Some(100)).unwrap();
        sink.apply_replay_row(&source.export_snapshot().unwrap(), 1);
        let update: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        luna_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
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
        let update: luna_doc::TranscriptUpdate = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let baseline = update.replay_baseline.unwrap();
        assert_eq!(baseline.entries.len(), 1);
        assert!(baseline.entries.contains_key("historical"));
        luna_doc::apply_transcript_frame(&mut entries, update.frame).unwrap();
        assert_eq!(entries.len(), 4);
        host.shutdown_workers().await;
    }

    #[tokio::test]
    async fn replay_preserves_each_watchers_opening_cutoff_without_consuming_live_text() {
        use crate::doc_host::{DocHost, DocHostConfig};
        use luna_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(luna_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: luna_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("cached-replay").unwrap();
        let sink =
            crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "cached-replay")
                .with_handle(Arc::downgrade(&handle));
        let source = luna_doc::SessionDoc::init("cached-replay").unwrap();
        let mut writer = luna_doc::SegmentWriter::begin(&source, "reply", "host", 0).unwrap();
        let text = |id: &str, value: &str| luna_doc::MessagePart::Text {
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
        let opening: luna_doc::TranscriptUpdate =
            serde_json::from_value(first.next().await.unwrap()).unwrap();
        assert_eq!(
            opening.replay_baseline.unwrap().entries["reply"]["body"],
            "café histórico".len()
        );

        let live = text("body", "café histórico y nuevo");
        writer.sync(&[live.clone()]).unwrap();
        sink.apply_row(&source.export_snapshot().unwrap(), 1);
        let update: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        let opening: luna_doc::TranscriptUpdate =
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
                let update: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        use luna_sync::chat_client::ChatDocSink;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(luna_sync::DocsStore::open(dir.path()).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: luna_proto::HarnessId::Mock,
                edge: None,
            },
        );
        let handle = host.open("reopen").unwrap();
        let sink = crate::chat2_host::EngineChatSink::new(&handle.doc_arc(), store, "reopen")
            .with_handle(Arc::downgrade(&handle));
        let source = luna_doc::SessionDoc::init("reopen").unwrap();
        let mut writer = luna_doc::SegmentWriter::begin(&source, "reply", "host", 0).unwrap();
        let part = |text: &str| luna_doc::MessagePart::Text {
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
        let update: luna_doc::TranscriptUpdate = serde_json::from_value(
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
        let host = luna_doc::SessionDoc::init("context-chat").unwrap();
        host.update_context_usage(Some(42000), Some(200000))
            .unwrap();
        // The viewing engine reads a replicated document, with no harness process.
        let remote = Arc::new(luna_doc::SessionDoc::from_doc(loro::LoroDoc::new()));
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

#[path = "board/mod.rs"]
pub(crate) mod board_engine;
#[path = "chat_workbench_rpc.rs"]
mod chat_workbench;
#[path = "child_threads.rs"]
mod child_threads;
