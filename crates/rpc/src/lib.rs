//! luna-rpc — the typed control plane (UiRpc / ControlRpc) over WebSocket + in-memory
//! transports, plus the device-room relay transport ({s,k,to,from} frames — [`device_room`]).
//!
//! Framing: ndjson envelopes, one JSON object per WebSocket text message (or per line on
//! byte transports), matching the shape of luna's Effect RPC without the Effect runtime:
//!
//! - client → server: `{id, method, params}` to invoke, `{id, cancel: true}` to stop a stream;
//! - server → client: `{id, ok}` / `{id, err}` for unary calls,
//!   `{id, item}`* then `{id, done: true}` (or `{id, err}`) for streams.
//!
//! The server dispatches into an [`RpcService`]; the [`RpcClient`] offers `call` and
//! `subscribe`. Both ends run over any pair of string channels, so the in-memory transport
//! ([`memory_client`]) exercises the exact same code path as the WebSocket one.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

mod client;
pub mod device_room;
pub mod remote_auth;
mod server;

pub use client::{RpcClient, RpcSubscription, connect_ws};
pub use device_room::{
    DeviceFrameHeader, DeviceLink, HostRelay, HostRelayConfig, LinkCache, LinkCacheConfig,
    PeerLiveness, PeerLivenessProbe, RemoteSessionSource, StaticToken, TokenError, TokenSource,
    decode_device_frame, device_room_ws_url, encode_device_frame,
};
pub use remote_auth::{RemoteAuthStore, RemoteEnvironment, RemoteScope};
pub use server::{serve_connection, serve_ws_listener, serve_ws_listener_with_factory};

/// RPC method names — single source of truth for both ends.
/// Full surface: docs/research/feature-inventory.md §2.
pub mod methods {
    // L29: only the local IPC owner can administer pairings and sessions.
    // Redeem/authenticate are handled by the guarded remote dispatch itself.
    pub const CREATE_REMOTE_PAIRING: &str = "CreateRemotePairing";
    pub const LIST_REMOTE_PAIRINGS: &str = "ListRemotePairings";
    pub const REVOKE_REMOTE_PAIRING: &str = "RevokeRemotePairing";
    pub const LIST_REMOTE_SESSIONS: &str = "ListRemoteSessions";
    pub const REVOKE_REMOTE_SESSION: &str = "RevokeRemoteSession";
    pub const REDEEM_REMOTE_PAIRING: &str = "RedeemRemotePairing";
    pub const AUTHENTICATE_REMOTE: &str = "AuthenticateRemote";
    /// L17 local-only backup control. Raw recovery material is accepted only
    /// by the explicit key-configure call and never returned.
    pub const GET_BACKUP_STATUS: &str = "GetBackupStatus";
    pub const CONFIGURE_BACKUP: &str = "ConfigureBackup";
    pub const CONFIGURE_BACKUP_KEY: &str = "ConfigureBackupKey";
    pub const BACKUP_NOW: &str = "BackupNow";
    pub const EXPORT_BACKUP: &str = "ExportBackup";
    pub const INSPECT_BACKUP: &str = "InspectBackup";
    pub const STAGE_BACKUP_RESTORE: &str = "StageBackupRestore";
    pub const WATCH_PREVIEWS: &str = "WatchPreviews";
    pub const LIST_HARNESSES: &str = "ListHarnesses";
    /// Per-device CLI versions and available updates; cached between checks.
    pub const GET_PROVIDER_UPDATES: &str = "GetProviderUpdates";
    /// Explicitly update one locally installed CLI through its install method.
    pub const UPDATE_PROVIDER: &str = "UpdateProvider";
    /// Flip a harness's enablement on the target device (Settings → Agents);
    /// replies with the device's fresh `ListHarnesses` catalog.
    pub const GET_TITLE_SETTINGS: &str = "GetTitleSettings";
    pub const SET_TITLE_SETTINGS: &str = "SetTitleSettings";
    pub const SET_HARNESS_ENABLED: &str = "SetHarnessEnabled";
    pub const LIST_MODELS: &str = "ListModels";
    pub const LIST_COMPOSER_CATALOG: &str = "ListComposerCatalog";
    pub const LIST_COMMANDS: &str = "ListCommands";
    pub const QUEUE_COMMAND: &str = "QueueCommand";
    /// Peer-to-peer delivery fallback: the SENDER's engine forwards a queued
    /// command entry (client-minted id and all) straight over the device-room
    /// link when its chat2 rows can't reach the edge but the host's peer link
    /// is alive. The host claims the id in its processed ledger before
    /// executing, so the doc row arriving later dedupes to a no-op —
    /// exactly-once by construction. Params `{chatId, entry}`.
    pub const RELAY_COMMAND: &str = "RelayCommand";
    /// User-driven delivery retry for a chat with unadopted queued sends:
    /// fresh chat2 socket, host nudge, drain pass, and a new delivery escort
    /// per pending command. Params `{chatId}`; IPC-only.
    pub const RETRY_DELIVERY: &str = "RetryDelivery";
    pub const CREATE_SIDE_CHAT: &str = "CreateSideChat";
    pub const LIST_SIDE_CHATS: &str = "ListSideChats";
    pub const READ_SIDE_CHAT: &str = "ReadSideChat";
    pub const SEND_SIDE_CHAT: &str = "SendSideChat";
    pub const ANSWER_SIDE_CHAT: &str = "AnswerSideChat";
    pub const UNLOAD_SIDE_CHAT: &str = "UnloadSideChat";
    /// Stop the side chat's current turn without ending the conversation.
    pub const STOP_SIDE_CHAT: &str = "StopSideChat";
    /// Compatibility read of side chat writability; persistent chats have no lease.
    pub const RENEW_SIDE_CHAT: &str = "RenewSideChat";
    pub const WATCH_DOC_MESSAGES: &str = "WatchDocMessages";
    /// Messages typed while the agent was busy, held on the chat doc so every
    /// device sees the same queue. `{ chatId }` → `{ items: QueuedMessage[] }`.
    pub const WATCH_QUEUE: &str = "WatchQueue";
    /// Provider-native workflows/teammates observed for a chat (read-only).
    /// `{ chatId }` → stream of `NativeTaskLedger`.
    pub const GET_TASK_WORK: &str = "GetTaskWork";
    pub const WATCH_TASK_WORK: &str = "WatchTaskWork";
    pub const PREVIEW_STOP_TASK_WORK: &str = "PreviewStopTaskWork";
    pub const STOP_TASK_WORK: &str = "StopTaskWork";
    pub const WATCH_NATIVE_TASKS: &str = "WatchNativeTasks";
    /// Claude Code's own session supervisor (`claude agents --json`), read-only.
    /// `{}` → `ClaudeExternalSessionsView`. IPC-only, engine-device local.
    pub const LIST_CLAUDE_EXTERNAL_SESSIONS: &str = "ListClaudeExternalSessions";
    /// Append to the queue. `{ chatId, text, attachments?, holdForTurnEnd? }` → `{ id }`.
    pub const QUEUE_MESSAGE: &str = "QueueMessage";
    /// Retype a queued message; empty text deletes it.
    /// `{ chatId, id, text }` → `{ changed }`.
    pub const UPDATE_QUEUED_MESSAGE: &str = "UpdateQueuedMessage";
    /// Acquire a host-authoritative edit lease for one queued row.
    pub const BEGIN_QUEUED_MESSAGE_EDIT: &str = "BeginQueuedMessageEdit";
    /// Renew an acquired queue edit lease.
    pub const RENEW_QUEUED_MESSAGE_EDIT: &str = "RenewQueuedMessageEdit";
    /// Commit, cancel, discard, or explicitly release an acquired edit.
    pub const FINISH_QUEUED_MESSAGE_EDIT: &str = "FinishQueuedMessageEdit";
    /// Reorder. `{ chatId, id, toIndex }` → `{ changed }`.
    pub const MOVE_QUEUED_MESSAGE: &str = "MoveQueuedMessage";
    pub const REMOVE_QUEUED_MESSAGE: &str = "RemoveQueuedMessage";
    /// Interrupt whatever is running and send this one. `{ chatId, id }` → `{ sent }`.
    pub const SEND_QUEUED_MESSAGE_NOW: &str = "SendQueuedMessageNow";
    /// Steer this row into the live turn without interrupting it.
    /// `{ chatId, id }` → `{ sent }`.
    pub const STEER_QUEUED_MESSAGE_NOW: &str = "SteerQueuedMessageNow";
    /// Nudge every open room client to verify liveness NOW (window focus,
    /// app foregrounded). No params; IPC-only. Each room ignores the hint
    /// unless it has been broadcast-quiet ≥30s, so this is cheap to spam.
    pub const PROBE_SYNC: &str = "ProbeSync";
    /// Live sync introspection (`luna sync` / debug surfaces): per-room
    /// connection state, last pushed-frame/ack ages, rejoin/probe/resync
    /// counters for the workspace room and every open chat doc. No params;
    /// IPC-only.
    pub const SYNC_STATUS: &str = "SyncStatus";
    /// Pushed edge-connectivity posture (`luna_proto::Connectivity`):
    /// current value first, then every change — the connection pill /
    /// composer-honesty / queued-badge feed. No params; IPC-only.
    pub const WATCH_CONNECTIVITY: &str = "WatchConnectivity";
    /// In-flight queued-attachment transfers (`luna_proto::TransferProgress`
    /// list): current set first, then a fresh snapshot per landed chunk —
    /// the sending thumbnail's percent-ring feed. No params; IPC-only.
    pub const WATCH_TRANSFERS: &str = "WatchTransfers";
    /// Visible project sessions; saved projectless rows remain in storage.
    pub const WATCH_CHATS: &str = "WatchChats";
    pub const WATCH_DEVICES: &str = "WatchDevices";
    pub const WATCH_SESSIONS: &str = "WatchSessions";
    /// Spaces registry (device+folder pairs) from the workspace doc.
    pub const WATCH_SPACES: &str = "WatchSpaces";
    /// Entity mutations against the workspace doc (feature-inventory §2 DataRpc).
    /// Params are tagged `{op: createChat|createSpace|renameSpace|deleteSpace|
    /// renameChat|setChatArchived|deleteChat|renameDevice|markChatSeen, …}`.
    /// `createChat` requires a `spaceId`.
    pub const MUTATE: &str = "Mutate";
    /// This engine's identity → `{deviceId}` (IPC-only; never relay-forwarded —
    /// the answer is about whichever engine you are directly connected to).
    pub const LOCAL_DEVICE: &str = "LocalDevice";
    /// This engine runtime's fixed device and workspace identity.
    pub const ENGINE_INFO: &str = "EngineInfo";
    /// Canonical data-root claim, served only by the local-owner IPC wrapper.
    /// Never register this on guarded remote transports.
    pub const LOCAL_OWNER_ROOT_CLAIM: &str = "LocalOwnerRootClaim";
    /// Readiness barrier for the engine runtime. The call completes once stores
    /// and journals are assembled, or fails with the assembly error.
    pub const ENGINE_READY: &str = "EngineReady";
    /// Durable LUNA task identity and all preserved execution-binding generations.
    pub const GET_TASK_IDENTITY: &str = "GetTaskIdentity";
    /// Local-only proof for a legacy workspace association; never starts a provider.
    pub const PREVIEW_WORKSPACE_IDENTITY_REPAIR: &str = "PreviewWorkspaceIdentityRepair";
    /// Redacted, provider-acknowledged LUNA runtime contract for one task.
    pub const GET_RUNTIME_CONTRACT: &str = "GetRuntimeContract";
    pub const PREVIEW_CONTEXT_TRANSFER: &str = "PreviewContextTransfer";
    pub const LIST_CONTEXT_TRANSFERS: &str = "ListContextTransfers";
    pub const GET_CONTEXT_TRANSFER: &str = "GetContextTransfer";
    pub const READ_CONTEXT_TRANSFER_ITEMS: &str = "ReadContextTransferItems";
    /// LUNA-owned global/project instruction settings, with revision readback.
    pub const GET_INSTRUCTION_SETTINGS: &str = "GetInstructionSettings";
    pub const SET_INSTRUCTION_SETTINGS: &str = "SetInstructionSettings";
    /// Read-only provider-owned skill/MCP configuration inventory.
    pub const GET_INTEGRATION_INVENTORY: &str = "GetIntegrationInventory";
    /// Owner-local: ask the provider (Claude) to save its own per-project MCP
    /// server enable/disable preference, with a fresh provider readback.
    pub const SET_PROVIDER_MCP_SERVER_ENABLED: &str = "SetProviderMcpServerEnabled";
    /// Owner-local: read one declared MCP Apps `ui://` resource as inert facts.
    pub const READ_PROVIDER_MCP_APP_RESOURCE: &str = "ReadProviderMcpAppResource";
    /// Effective native settings sources, output style, hooks, commands and
    /// skills for the launch a chat/project would use (local device only).
    pub const GET_NATIVE_EXTENSIONS: &str = "GetNativeExtensions";
    /// Provider-owned SKILL.md library on the engine's local device.
    pub const GET_SKILL_DOCUMENT: &str = "GetSkillDocument";
    pub const SAVE_SKILL_DOCUMENT: &str = "SaveSkillDocument";
    pub const INSTALL_SKILL: &str = "InstallSkill";
    pub const REMOVE_SKILL: &str = "RemoveSkill";
    /// Bounded search over the local derived session-activity index.
    pub const SEARCH_SESSION_HISTORY: &str = "SearchSessionHistory";
    /// L16 normalized local usage. The runtime accepts only explicit synthetic homes.
    pub const GET_USAGE_SNAPSHOT: &str = "GetUsageSnapshot";
    /// Rebuild the local derived index from authoritative task records.
    pub const REBUILD_SESSION_HISTORY: &str = "RebuildSessionHistory";
    /// Read/write project-local indexing exclusions and sensitive-surface policy.
    pub const GET_SESSION_HISTORY_POLICY: &str = "GetSessionHistoryPolicy";
    pub const SET_SESSION_HISTORY_POLICY: &str = "SetSessionHistoryPolicy";
    /// L15 local minimized Activity projection. These calls are deliberately
    /// not relay-forwarded while remote grants and scopes are unsupported.
    pub const GET_ATTENTION: &str = "GetAttention";
    pub const CLAIM_ATTENTION_TRANSITIONS: &str = "ClaimAttentionTransitions";
    pub const MUTATE_ATTENTION: &str = "MutateAttention";
    /// Read the current provider-native history fork capability for one task.
    pub const GET_HISTORY_CAPABILITY: &str = "GetHistoryCapability";
    /// Read durable pre-edit recovery snapshots available for one task.
    pub const LIST_HISTORY_RECOVERIES: &str = "ListHistoryRecoveries";
    /// CC-10 dry-run a provider-native file checkpoint restore (Claude).
    pub const PREVIEW_NATIVE_FILE_REWIND: &str = "PreviewNativeFileRewind";
    /// CC-10 restore provider-checkpointed files after a matching preview.
    pub const APPLY_NATIVE_FILE_REWIND: &str = "ApplyNativeFileRewind";
    /// Explicit supported-provider refresh for one task binding.
    pub const REFRESH_TASK_BINDING: &str = "RefreshTaskBinding";
    /// Read-only provider-native import preview. No task or provider state is mutated.
    pub const PREVIEW_PROVIDER_TASK_IMPORT: &str = "PreviewProviderTaskImport";
    /// Adopt a current preview; task and binding persist before chat visibility.
    pub const ADOPT_PROVIDER_TASK: &str = "AdoptProviderTask";
    /// L07 authoritative path for every existing local task mutation.
    pub const CREATE_CHILD_THREAD: &str = "CreateChildThread";
    pub const CREATE_CHILD_TAB: &str = "CreateChildTab";
    /// Device/profile-local presentation state; never relay-forwarded.
    pub const GET_CHAT_WORKBENCH: &str = "GetChatWorkbench";
    pub const WATCH_CHAT_WORKBENCH: &str = "WatchChatWorkbench";
    pub const MUTATE_CHAT_WORKBENCH: &str = "MutateChatWorkbench";
    pub const EXECUTE_TASK_COMMAND: &str = "ExecuteTaskCommand";
    pub const GET_TASK_COMMAND_RECEIPT: &str = "GetTaskCommandReceipt";
    pub const GET_TASK_CLEANUP: &str = "GetTaskCleanup";
    pub const GET_PROJECT_CLEANUP: &str = "GetProjectCleanup";
    pub const GET_DELETION_CLEANUP_SUMMARY: &str = "GetDeletionCleanupSummary";
    pub const GET_TASK_COMMAND_SNAPSHOT: &str = "GetTaskCommandSnapshot";
    pub const WATCH_TASK_COMMANDS: &str = "WatchTaskCommands";
    /// L08 composing-device-local durable draft surface. These methods are
    /// intentionally not relay-forwardable or synced across devices.
    pub const GET_COMPOSER_DRAFT: &str = "GetComposerDraft";
    pub const SAVE_COMPOSER_DRAFT: &str = "SaveComposerDraft";
    pub const LIST_PROMPT_STASH: &str = "ListPromptStash";
    pub const SAVE_PROMPT_STASH: &str = "SavePromptStash";
    pub const DELETE_PROMPT_STASH: &str = "DeletePromptStash";
    pub const BEGIN_DRAFT_SUBMISSION: &str = "BeginDraftSubmission";
    pub const SETTLE_DRAFT_SUBMISSION: &str = "SettleDraftSubmission";
    /// Ask a headless IPC owner to drain its runtime and exit successfully.
    /// Headed IPC owners do not implement this method: closing another app's
    /// engine behind its windows would leave that process unusable.
    pub const STOP_ENGINE: &str = "StopEngine";
    pub const AUTH_STATUS: &str = "AuthStatus";
    /// Metadata-only inventory of LUNA-owned credentials. No value getter exists.
    pub const LIST_CREDENTIAL_METADATA: &str = "ListCredentialMetadata";
    // AuthRpc mutations (feature-inventory §2 AuthRpc; IPC-only).
    pub const SIGN_IN: &str = "SignIn";
    pub const SIGN_IN_HEADLESS: &str = "SignInHeadless";
    pub const COMPLETE_SIGN_IN: &str = "CompleteSignIn";
    pub const SIGN_OUT: &str = "SignOut";
    pub const LIST_ORGS: &str = "ListOrgs";
    pub const CREATE_ORG: &str = "CreateOrg";
    pub const SELECT_ORG: &str = "SelectOrg";
    /// One-time local→synced profile import: what's importable (unary).
    pub const LOCAL_IMPORT_STATUS: &str = "LocalImportStatus";
    /// One-time local→synced profile import: run it (stream of progress items).
    pub const IMPORT_LOCAL_WORKSPACE: &str = "ImportLocalWorkspace";
    // Repos / worktrees / folders (ControlRpc, relay-forwardable).
    pub const LIST_REPOS: &str = "ListRepos";
    pub const ADD_REPO: &str = "AddRepo";
    pub const CLONE_REPO: &str = "CloneRepo";
    pub const CREATE_REPO: &str = "CreateRepo";
    pub const LIST_BRANCHES: &str = "ListBranches";
    pub const LIST_REFS: &str = "ListRefs";
    pub const LIST_GIT_HISTORY: &str = "ListGitHistory";
    /// Fuzzy commit-subject / SHA search over the complete public history.
    pub const SEARCH_GIT_HISTORY: &str = "SearchGitHistory";
    /// Resolve hosted profile images for a page of Git commit authors.
    pub const RESOLVE_GIT_AVATARS: &str = "ResolveGitAvatars";
    /// Update remote-tracking refs without changing HEAD, the index, or files.
    pub const FETCH_ALL: &str = "FetchAll";
    pub const SWITCH_REF: &str = "SwitchRef";
    pub const LIST_FOLDERS: &str = "ListFolders";
    /// The device's browse roots: home plus mounted drives/volumes.
    pub const LIST_DRIVES: &str = "ListDrives";
    /// Fuzzy relative-path search rooted in a known chat or space checkout.
    pub const SEARCH_FILES: &str = "SearchFiles";
    // Device-local workspace filesystem operations. All are relay-forwardable;
    // WatchWorkspaceFiles is the only streaming method in this group.
    pub const LIST_WORKSPACE_DIRECTORY: &str = "ListWorkspaceDirectory";
    pub const SEARCH_WORKSPACE_FILES: &str = "SearchWorkspaceFiles";
    pub const READ_WORKSPACE_IMAGE: &str = "ReadWorkspaceImage";
    pub const READ_WORKSPACE_FILE: &str = "ReadWorkspaceFile";
    pub const WRITE_WORKSPACE_FILE: &str = "WriteWorkspaceFile";
    pub const WATCH_WORKSPACE_FILES: &str = "WatchWorkspaceFiles";
    pub const CREATE_WORKTREE: &str = "CreateWorktree";
    pub const DELETE_WORKTREE: &str = "DeleteWorktree";
    // Terminals (ControlRpc, relay-forwardable; SubscribeTerminal streams).
    pub const GET_PROJECT_COMMANDS: &str = "GetProjectCommands";
    pub const SAVE_PROJECT_COMMANDS: &str = "SaveProjectCommands";
    pub const PROJECT_COMMAND_STATUS: &str = "ProjectCommandStatus";
    pub const RUN_PROJECT_COMMAND: &str = "RunProjectCommand";
    pub const STOP_PROJECT_COMMAND: &str = "StopProjectCommand";
    pub const RETRY_PROJECT_COMMAND: &str = "RetryProjectCommand";
    pub const OPEN_TERMINAL: &str = "OpenTerminal";
    pub const SUBSCRIBE_TERMINAL: &str = "SubscribeTerminal";
    pub const WRITE_TERMINAL: &str = "WriteTerminal";
    pub const RESIZE_TERMINAL: &str = "ResizeTerminal";
    pub const CLOSE_TERMINAL: &str = "CloseTerminal";
    pub const LIST_TERMINALS: &str = "ListTerminals";
    /// Checkout-diff stream for the target device's chats (DataRpc,
    /// relay-forwardable — diffs are produced where the checkout lives).
    pub const WATCH_CHECKOUT_DIFFS: &str = "WatchCheckoutDiffs";
    /// Current pull request for one checkout, resolved on the checkout's host device.
    pub const WATCH_CHECKOUT_CHANGE_REQUEST: &str = "WatchCheckoutChangeRequest";
    pub const GET_CHECKOUT_DIFF: &str = "GetCheckoutDiff";
    pub const GET_CHECKOUT_FILE_DIFF_TEXT: &str = "GetCheckoutFileDiffText";
    pub const GET_GIT_ACTION_PREVIEW: &str = "GetGitActionPreview";
    pub const EXECUTE_GIT_ACTION: &str = "ExecuteGitAction";
    pub const GENERATE_GIT_COMMIT_MESSAGE: &str = "GenerateGitCommitMessage";
    pub const GET_GITHUB_PR_PREVIEW: &str = "GetGitHubPrPreview";
    pub const GET_GITHUB_PR_DETAILS: &str = "GetGitHubPrDetails";
    pub const CREATE_GITHUB_DRAFT_PR: &str = "CreateGitHubDraftPr";
    pub const GET_GIT_PR_STATUS: &str = "GetGitPrStatus";
    pub const LIST_GIT_CHECKPOINTS: &str = "ListGitCheckpoints";
    pub const DIFF_GIT_CHECKPOINT: &str = "DiffGitCheckpoint";
    pub const PREVIEW_GIT_CHECKPOINT_RESTORE: &str = "PreviewGitCheckpointRestore";
    pub const RESTORE_GIT_CHECKPOINT: &str = "RestoreGitCheckpoint";
    // Agent account metadata is relay-readable; credential/login actions stay local.
    pub const LIST_AGENT_ACCOUNTS: &str = "ListAgentAccounts";
    pub const ACTIVATE_AGENT_ACCOUNT: &str = "ActivateAgentAccount";
    pub const RENAME_AGENT_ACCOUNT: &str = "RenameAgentAccount";
    pub const FORGET_AGENT_ACCOUNT: &str = "ForgetAgentAccount";
    pub const START_AGENT_LOGIN: &str = "StartAgentLogin";
    pub const COMPLETE_AGENT_LOGIN: &str = "CompleteAgentLogin";
    pub const POLL_AGENT_LOGIN: &str = "PollAgentLogin";
    pub const CANCEL_AGENT_LOGIN: &str = "CancelAgentLogin";
    // Uploads / attachments (ControlRpc, relay-forwardable — target the chat's host device).
    pub const UPLOAD_CHUNK: &str = "UploadChunk";
    pub const UPLOAD_COMMIT: &str = "UploadCommit";
    pub const READ_ATTACHMENT_CHUNK: &str = "ReadAttachmentChunk";
    /// Lazy full-tool-output fetch by doc-resident ref: read the owning
    /// host's journal first, then a legacy R2 sidecar. Relay-forwardable.
    pub const FETCH_TOOL_BLOB: &str = "FetchToolBlob";
    /// Owner-only live prompt URL fetch; remote read scopes cannot retrieve it.
    pub const MCP_ELICITATION_URL: &str = "McpElicitationUrl";
    // Updates (ControlRpc, relay-forwardable — a device reports/applies its own
    // binary's update). Stream: current UpdateStatus, then every change.
    pub const UPDATE_STATUS: &str = "UpdateStatus";
    /// Download + apply the newest release on the target device (symlink-managed
    /// installs; the service restart is scheduled after the reply flushes).
    pub const APPLY_UPDATE: &str = "ApplyUpdate";
    /// Inventory of shipped RPC names. Remote policy treats unlisted names as unknown.
    pub const ALL: &[&str] = &[
        CREATE_REMOTE_PAIRING,
        LIST_REMOTE_PAIRINGS,
        REVOKE_REMOTE_PAIRING,
        LIST_REMOTE_SESSIONS,
        REVOKE_REMOTE_SESSION,
        REDEEM_REMOTE_PAIRING,
        AUTHENTICATE_REMOTE,
        GET_BACKUP_STATUS,
        CONFIGURE_BACKUP,
        CONFIGURE_BACKUP_KEY,
        BACKUP_NOW,
        EXPORT_BACKUP,
        INSPECT_BACKUP,
        STAGE_BACKUP_RESTORE,
        WATCH_PREVIEWS,
        LIST_HARNESSES,
        GET_PROVIDER_UPDATES,
        UPDATE_PROVIDER,
        GET_TITLE_SETTINGS,
        SET_TITLE_SETTINGS,
        SET_HARNESS_ENABLED,
        LIST_MODELS,
        LIST_COMMANDS,
        LIST_COMPOSER_CATALOG,
        GET_PROJECT_COMMANDS,
        SAVE_PROJECT_COMMANDS,
        RUN_PROJECT_COMMAND,
        RETRY_PROJECT_COMMAND,
        STOP_PROJECT_COMMAND,
        PROJECT_COMMAND_STATUS,
        QUEUE_COMMAND,
        RELAY_COMMAND,
        RETRY_DELIVERY,
        CREATE_SIDE_CHAT,
        LIST_SIDE_CHATS,
        READ_SIDE_CHAT,
        SEND_SIDE_CHAT,
        ANSWER_SIDE_CHAT,
        UNLOAD_SIDE_CHAT,
        STOP_SIDE_CHAT,
        RENEW_SIDE_CHAT,
        WATCH_DOC_MESSAGES,
        WATCH_QUEUE,
        GET_TASK_WORK,
        WATCH_TASK_WORK,
        PREVIEW_STOP_TASK_WORK,
        STOP_TASK_WORK,
        WATCH_NATIVE_TASKS,
        LIST_CLAUDE_EXTERNAL_SESSIONS,
        QUEUE_MESSAGE,
        UPDATE_QUEUED_MESSAGE,
        BEGIN_QUEUED_MESSAGE_EDIT,
        RENEW_QUEUED_MESSAGE_EDIT,
        FINISH_QUEUED_MESSAGE_EDIT,
        MOVE_QUEUED_MESSAGE,
        REMOVE_QUEUED_MESSAGE,
        SEND_QUEUED_MESSAGE_NOW,
        STEER_QUEUED_MESSAGE_NOW,
        PROBE_SYNC,
        SYNC_STATUS,
        WATCH_CONNECTIVITY,
        WATCH_TRANSFERS,
        WATCH_CHATS,
        WATCH_DEVICES,
        WATCH_SESSIONS,
        WATCH_SPACES,
        MUTATE,
        LOCAL_DEVICE,
        ENGINE_INFO,
        LOCAL_OWNER_ROOT_CLAIM,
        ENGINE_READY,
        GET_TASK_IDENTITY,
        PREVIEW_WORKSPACE_IDENTITY_REPAIR,
        GET_RUNTIME_CONTRACT,
        PREVIEW_CONTEXT_TRANSFER,
        LIST_CONTEXT_TRANSFERS,
        GET_CONTEXT_TRANSFER,
        READ_CONTEXT_TRANSFER_ITEMS,
        GET_INSTRUCTION_SETTINGS,
        SET_INSTRUCTION_SETTINGS,
        GET_INTEGRATION_INVENTORY,
        SET_PROVIDER_MCP_SERVER_ENABLED,
        READ_PROVIDER_MCP_APP_RESOURCE,
        GET_NATIVE_EXTENSIONS,
        GET_SKILL_DOCUMENT,
        SAVE_SKILL_DOCUMENT,
        INSTALL_SKILL,
        REMOVE_SKILL,
        SEARCH_SESSION_HISTORY,
        GET_USAGE_SNAPSHOT,
        REBUILD_SESSION_HISTORY,
        GET_SESSION_HISTORY_POLICY,
        SET_SESSION_HISTORY_POLICY,
        GET_ATTENTION,
        CLAIM_ATTENTION_TRANSITIONS,
        MUTATE_ATTENTION,
        GET_HISTORY_CAPABILITY,
        LIST_HISTORY_RECOVERIES,
        PREVIEW_NATIVE_FILE_REWIND,
        APPLY_NATIVE_FILE_REWIND,
        REFRESH_TASK_BINDING,
        PREVIEW_PROVIDER_TASK_IMPORT,
        ADOPT_PROVIDER_TASK,
        CREATE_CHILD_THREAD,
        CREATE_CHILD_TAB,
        GET_CHAT_WORKBENCH,
        WATCH_CHAT_WORKBENCH,
        MUTATE_CHAT_WORKBENCH,
        EXECUTE_TASK_COMMAND,
        GET_TASK_COMMAND_RECEIPT,
        GET_TASK_CLEANUP,
        GET_PROJECT_CLEANUP,
        GET_DELETION_CLEANUP_SUMMARY,
        GET_TASK_COMMAND_SNAPSHOT,
        WATCH_TASK_COMMANDS,
        GET_COMPOSER_DRAFT,
        SAVE_COMPOSER_DRAFT,
        LIST_PROMPT_STASH,
        SAVE_PROMPT_STASH,
        DELETE_PROMPT_STASH,
        BEGIN_DRAFT_SUBMISSION,
        SETTLE_DRAFT_SUBMISSION,
        STOP_ENGINE,
        AUTH_STATUS,
        LIST_CREDENTIAL_METADATA,
        SIGN_IN,
        SIGN_IN_HEADLESS,
        COMPLETE_SIGN_IN,
        SIGN_OUT,
        LIST_ORGS,
        CREATE_ORG,
        SELECT_ORG,
        LOCAL_IMPORT_STATUS,
        IMPORT_LOCAL_WORKSPACE,
        LIST_REPOS,
        ADD_REPO,
        CLONE_REPO,
        CREATE_REPO,
        LIST_BRANCHES,
        LIST_REFS,
        LIST_GIT_HISTORY,
        SEARCH_GIT_HISTORY,
        RESOLVE_GIT_AVATARS,
        FETCH_ALL,
        SWITCH_REF,
        LIST_FOLDERS,
        LIST_DRIVES,
        SEARCH_FILES,
        LIST_WORKSPACE_DIRECTORY,
        SEARCH_WORKSPACE_FILES,
        READ_WORKSPACE_IMAGE,
        READ_WORKSPACE_FILE,
        WRITE_WORKSPACE_FILE,
        WATCH_WORKSPACE_FILES,
        CREATE_WORKTREE,
        DELETE_WORKTREE,
        OPEN_TERMINAL,
        SUBSCRIBE_TERMINAL,
        WRITE_TERMINAL,
        RESIZE_TERMINAL,
        CLOSE_TERMINAL,
        LIST_TERMINALS,
        WATCH_CHECKOUT_DIFFS,
        WATCH_CHECKOUT_CHANGE_REQUEST,
        GET_CHECKOUT_DIFF,
        GET_CHECKOUT_FILE_DIFF_TEXT,
        GET_GIT_ACTION_PREVIEW,
        EXECUTE_GIT_ACTION,
        GENERATE_GIT_COMMIT_MESSAGE,
        GET_GIT_PR_STATUS,
        GET_GITHUB_PR_PREVIEW,
        GET_GITHUB_PR_DETAILS,
        CREATE_GITHUB_DRAFT_PR,
        LIST_GIT_CHECKPOINTS,
        DIFF_GIT_CHECKPOINT,
        PREVIEW_GIT_CHECKPOINT_RESTORE,
        RESTORE_GIT_CHECKPOINT,
        LIST_AGENT_ACCOUNTS,
        ACTIVATE_AGENT_ACCOUNT,
        RENAME_AGENT_ACCOUNT,
        FORGET_AGENT_ACCOUNT,
        START_AGENT_LOGIN,
        COMPLETE_AGENT_LOGIN,
        POLL_AGENT_LOGIN,
        CANCEL_AGENT_LOGIN,
        UPLOAD_CHUNK,
        UPLOAD_COMMIT,
        READ_ATTACHMENT_CHUNK,
        FETCH_TOOL_BLOB,
        MCP_ELICITATION_URL,
        UPDATE_STATUS,
        APPLY_UPDATE,
    ];
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("bad params: {0}")]
    BadParams(String),
    #[error("{0}")]
    Failed(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("connection closed")]
    Closed,
}

/// A client-originated frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientFrame {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancel: bool,
}

/// A server-originated frame. Exactly one of `ok` / `err` / `item` / `done` is meaningful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerFrame {
    pub id: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json_value"
    )]
    pub ok: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json_value"
    )]
    pub item: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

// A present `ok: null` is a successful nullable RPC result, not a missing
// response field. Serde's default Option<Value> decoder collapses both to None.
fn deserialize_present_json_value<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// What a service returns for one invocation.
pub enum RpcReply {
    /// Unary response — sent as `{id, ok}`.
    Value(serde_json::Value),
    /// Stream — each item sent as `{id, item}`, then `{id, done: true}` when it ends.
    Stream(BoxStream<'static, serde_json::Value>),
}

impl RpcReply {
    /// Serialize a value into a unary reply.
    pub fn value<T: Serialize>(value: &T) -> Result<Self, RpcError> {
        serde_json::to_value(value)
            .map(RpcReply::Value)
            .map_err(|e| RpcError::Failed(format!("serialize response: {e}")))
    }
}

/// Server-side dispatch: one implementation serves every transport.
#[async_trait]
pub trait RpcService: Send + Sync + 'static {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError>;
}

/// Deserialize typed params out of the envelope's `params` value.
pub fn parse_params<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))
}

/// Spawn an in-memory server for `service` and return a connected client.
/// Same envelopes, same dispatch loop as the WebSocket path — the in-process UI
/// transport (ARCHITECTURE §1 "zero serialization shortcuts").
pub fn memory_client(service: Arc<dyn RpcService>) -> RpcClient {
    let (client_out, server_in) = tokio::sync::mpsc::channel::<String>(256);
    let (server_out, client_in) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, server_out, server_in));
    RpcClient::new(client_out, client_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Mutex;

    struct TestService;

    struct CancelAwareService {
        dropped: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[async_trait]
    impl RpcService for CancelAwareService {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != methods::WATCH_CHECKOUT_CHANGE_REQUEST {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let guard = DropSignal(self.dropped.lock().unwrap().take());
            let stream = futures::stream::unfold(guard, |guard| async move {
                let item = std::future::pending::<Option<(serde_json::Value, DropSignal)>>().await;
                drop(guard);
                item
            });
            Ok(RpcReply::Stream(stream.boxed()))
        }
    }

    #[async_trait]
    impl RpcService for TestService {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                "Echo" => Ok(RpcReply::Value(params)),
                "Count" => {
                    let n = params.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                    Ok(RpcReply::Stream(
                        futures::stream::iter((0..n).map(|i| serde_json::json!(i))).boxed(),
                    ))
                }
                "Never" => Ok(RpcReply::Stream(futures::stream::pending().boxed())),
                "Boom" => Err(RpcError::Failed("boom".into())),
                "RelayLost" => Err(RpcError::Transport("synthetic relay loss".into())),
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    #[tokio::test]
    async fn memory_call_stream_and_error() {
        let client = memory_client(Arc::new(TestService));

        let echoed = client
            .call("Echo", serde_json::json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(echoed, serde_json::json!({"x": 1}));

        let mut items = client
            .subscribe("Count", serde_json::json!({"n": 3}))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(v) = items.recv().await {
            seen.push(v);
        }
        assert_eq!(
            seen,
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );

        let err = client
            .call("Boom", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, RpcError::Failed(m) if m == "boom"));
    }

    #[tokio::test]
    async fn nullable_unary_result_completes() {
        let client = memory_client(Arc::new(TestService));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.call_as::<Option<u64>>("Echo", serde_json::Value::Null),
        )
        .await
        .expect("null reply must settle the pending call")
        .unwrap();
        assert_eq!(result, None);
        let frame: ServerFrame = serde_json::from_str(r#"{"id":1,"ok":null}"#).unwrap();
        assert_eq!(frame.ok, Some(serde_json::Value::Null));
        let absent: ServerFrame = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert_eq!(absent.ok, None);
    }

    #[tokio::test]
    async fn transport_error_preserves_retryable_kind() {
        let client = memory_client(Arc::new(TestService));
        let error = client
            .call("RelayLost", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(error, RpcError::Transport(message) if message == "synthetic relay loss"));
    }

    #[tokio::test]
    async fn checked_stream_acknowledges_support_and_preserves_unknown_method() {
        let client = memory_client(Arc::new(TestService));

        let mut items = client
            .subscribe_checked("Count", serde_json::json!({"n": 1}))
            .await
            .unwrap();
        assert_eq!(items.recv().await, Some(serde_json::json!(0)));
        assert_eq!(items.recv().await, None);

        let error = match client
            .subscribe_checked("FutureStream", serde_json::Value::Null)
            .await
        {
            Ok(_) => panic!("old service must reject unknown stream"),
            Err(error) => error,
        };
        assert!(matches!(error, RpcError::UnknownMethod(method) if method == "FutureStream"));
    }

    #[tokio::test]
    async fn dropping_checked_subscription_cancels_pending_server_stream() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let client = memory_client(Arc::new(CancelAwareService {
            dropped: Mutex::new(Some(dropped_tx)),
        }));
        let stream = client
            .subscribe_checked(
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                serde_json::Value::Null,
            )
            .await
            .unwrap();

        drop(stream);

        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("server stream cancelled")
            .expect("drop signal");
    }

    #[tokio::test]
    async fn websocket_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(serve_ws_listener(listener, Arc::new(TestService)));

        let client = connect_ws(&format!("ws://127.0.0.1:{port}")).await.unwrap();
        let echoed = client
            .call("Echo", serde_json::json!("hello"))
            .await
            .unwrap();
        assert_eq!(echoed, serde_json::json!("hello"));

        let mut items = client
            .subscribe("Count", serde_json::json!({"n": 2}))
            .await
            .unwrap();
        assert_eq!(items.recv().await, Some(serde_json::json!(0)));
        assert_eq!(items.recv().await, Some(serde_json::json!(1)));
        assert_eq!(items.recv().await, None);
    }

    #[tokio::test]
    async fn handshake_with_origin_header_is_rejected() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(serve_ws_listener(listener, Arc::new(TestService)));

        // A browser page opening ws://127.0.0.1:{port} always sends Origin;
        // the server must refuse the handshake before serving any RPC.
        let mut req = format!("ws://127.0.0.1:{port}")
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("origin", "https://evil.example".parse().unwrap());
        let result = tokio_tungstenite::connect_async(req).await;
        assert!(
            result.is_err(),
            "handshake carrying an Origin header must be rejected"
        );

        // A native viewport (no Origin) still connects and can call RPC — the
        // reject must not be a blanket denial.
        let client = connect_ws(&format!("ws://127.0.0.1:{port}")).await.unwrap();
        let echoed = client.call("Echo", serde_json::json!("ok")).await.unwrap();
        assert_eq!(echoed, serde_json::json!("ok"));
    }

    #[tokio::test]
    async fn dropping_stream_receiver_cancels_server_side() {
        let client = memory_client(Arc::new(TestService));
        let items = client
            .subscribe("Never", serde_json::Value::Null)
            .await
            .unwrap();
        drop(items);
        // The next unary call still works — the dead stream didn't wedge the connection.
        let echoed = client.call("Echo", serde_json::json!(2)).await.unwrap();
        assert_eq!(echoed, serde_json::json!(2));
    }
}
